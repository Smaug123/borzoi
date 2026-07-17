//! FCS-side projector: walk the AdjacentTag JSON produced by `fcs-dump ast`
//! into the normalised model.

use std::path::Path;

use serde_json::Value;

use super::decode::decode_base64;
use super::model::*;

// ============================================================================
// FCS-side projector: walk the AdjacentTag JSON produced by `fcs-dump ast`.
// ============================================================================
//
// AdjacentTag encoding for an F# DU case `Foo of int * string` is
// `{"Case": "Foo", "Fields": [<int>, <string>]}`. Records ride through as
// objects with named fields. We do all the matching by hand on
// `serde_json::Value` rather than typed serde — the FCS AST is too large
// (~60 DUs in `SyntaxTree.fsi`) and shape-unstable across F# versions to
// be worth a typed deserialiser; we only project the slice Phase 1 cares
// about.

/// Project only the `ParseTree` portion of an `fcs-dump ast` payload.
///
/// This intentionally does not decide whether FCS accepted the source:
/// recovery-AST tests use the tree even when `ParseHadErrors` is true. Callers
/// that mean "FCS accepts" must check `ParseHadErrors` before comparing trees.
pub fn normalise_fcs_dump(json: &str) -> NormalisedRoot {
    let dump: Value = serde_json::from_str(json).expect("fcs-dump JSON shape");
    let parse_tree = dump
        .get("ParseTree")
        .expect("fcs-dump payload missing ParseTree");
    fcs_parsed_input(parse_tree)
}

fn fcs_parsed_input(v: &Value) -> NormalisedRoot {
    let case = case_name(v);
    match case {
        "ImplFile" => {
            let fields = fields(v);
            let impl_file = &fields[0];
            NormalisedRoot::Impl(fcs_impl_file(impl_file))
        }
        "SigFile" => {
            let fields = fields(v);
            let sig_file = &fields[0];
            NormalisedRoot::Sig(fcs_sig_file(sig_file))
        }
        other => panic!("unknown ParsedInput case {other:?}"),
    }
}

/// `ParsedSigFileInput(fileName, qualifiedNameOfFile, hashDirectives,
/// **contents** [field 3], trivia, identifiers)` (phase 10.11). The `contents`
/// list is `SynModuleOrNamespaceSig`, whose fields match
/// `SynModuleOrNamespace` exactly — so the header projection is shared via
/// [`fcs_module_header`].
fn fcs_sig_file(v: &Value) -> NormalisedSigFile {
    let fields = fields(v);
    let warn_directives = fcs_warn_directives(&fields[4]);
    let modules = fields[3]
        .as_array()
        .expect("ParsedSigFileInput.contents must be array");
    NormalisedSigFile {
        warn_directives,
        modules: modules.iter().map(fcs_sig_module).collect(),
    }
}

fn fcs_sig_module(v: &Value) -> NormalisedSigModule {
    let (kind, is_rec, attributes, access) = fcs_module_header(v);
    let decls = fields(v)[3]
        .as_array()
        .expect("SynModuleOrNamespaceSig.decls must be array");
    NormalisedSigModule {
        kind,
        is_rec,
        attributes,
        access,
        decls: decls.iter().map(fcs_sig_decl).collect(),
    }
}

/// Project one `SynModuleSigDecl` (Block D). Phase 10.13a handles `Open(target,
/// range)`; 10.13b adds `NestedModule` and `ModuleAbbrev` — both share the
/// impl-side `SynModuleDecl` field layout, so the projections mirror [`fcs_decl`].
fn fcs_sig_decl(v: &Value) -> NormalisedSigDecl {
    match case_name(v) {
        "Open" => {
            let target = &fields(v)[0];
            let normalised = match case_name(target) {
                "ModuleOrNamespace" => NormalisedOpenTarget::ModuleOrNamespace(
                    fcs_syn_long_ident_segments(&fields(target)[0]),
                ),
                "Type" => NormalisedOpenTarget::Type(fcs_type(&fields(target)[0])),
                other => panic!("unknown SynOpenDeclTarget case {other:?}"),
            };
            NormalisedSigDecl::Open { target: normalised }
        }
        "NestedModule" => {
            // `SynModuleSigDecl.NestedModule(moduleInfo: SynComponentInfo,
            // isRecursive, moduleDecls: SynModuleSigDecl list, range, trivia)` —
            // same leading fields as the impl `SynModuleDecl.NestedModule`. Name
            // is `SynComponentInfo.longId` (field 3); attrs field 0 (phase 10.7d).
            let f = fields(v);
            let module_info = fields(&f[0]);
            let attributes = fcs_attribute_lists(&module_info[0]);
            let long_id = fcs_ident_list_texts(&module_info[3]);
            // `SynComponentInfo.accessibility` (field 6).
            let access = fcs_access(&module_info[6]);
            let is_rec = f[1]
                .as_bool()
                .expect("SynModuleSigDecl.NestedModule field 1 (isRecursive) must be a JSON bool");
            let decls = f[2]
                .as_array()
                .expect("SynModuleSigDecl.NestedModule field 2 (moduleDecls) must be array")
                .iter()
                .map(fcs_sig_decl)
                .collect();
            NormalisedSigDecl::NestedModule {
                long_id,
                is_rec,
                attributes,
                access,
                decls,
            }
        }
        "ModuleAbbrev" => {
            // `SynModuleSigDecl.ModuleAbbrev(ident: Ident, longId: LongIdent,
            // range)` — identical to the impl form. Field 0 the single LHS
            // `Ident`; field 1 the RHS `Ident list`.
            let f = fields(v);
            let ident = f[0]
                .get("idText")
                .and_then(Value::as_str)
                .expect("SynModuleSigDecl.ModuleAbbrev field 0 (ident) has idText")
                .to_string();
            NormalisedSigDecl::ModuleAbbrev {
                ident,
                long_id: fcs_ident_list_texts(&f[1]),
            }
        }
        "Val" => {
            // `SynModuleSigDecl.Val(valSig: SynValSig, range)` (phase 10.12a) —
            // field 0 the `SynValSig`, projected to (name, type) via the shared
            // `fcs_val_sig` (also used by the abstract slot, 9.10c). The
            // `SynValSig`'s own field 0 is `attributes` (`[<Literal>] val …`).
            let val_sig = &fields(v)[0];
            let attributes = fcs_attribute_lists(&fields(val_sig)[0]);
            // `SynValSig.accessibility` (field 8, a `SynValSigAccess`).
            let access = fcs_val_sig_access(&fields(val_sig)[8]);
            let (name, ty) = fcs_val_sig(val_sig);
            // Explicit value typars (`val f<'T> : …`, phase 10.12) — `SynValSig`
            // field 2 is a `SynValTyparDecls(typarDecls: SynTyparDecls option,
            // canInfer)` record; its field 0 is the `SynTyparDecls option`, read
            // the same way a type-definition header's `typeParams` is. The
            // inside-`<>` `when` constraints come from that same `PostfixList`
            // (there is no after-decls constraint slot at the typar position — the
            // after-type `when` lives in the signature `ty`).
            let syn_val_typar_decls = &fields(val_sig)[2];
            let typar_decls_opt = &fields(syn_val_typar_decls)[0];
            let typars = fcs_typar_decls(typar_decls_opt);
            let constraints = fcs_type_defn_constraints(typar_decls_opt, &Value::Null);
            // The `= <literal>` value (`val x : int = 1`, phase 10.12) —
            // `SynValSig.synExpr` (field 9, a `SynExpr option`), projected via the
            // shared expression normaliser. `null` (None) for no literal.
            let literal = match &fields(val_sig)[9] {
                Value::Null => None,
                some => Some(Box::new(fcs_expr(&fields(some)[0]))),
            };
            NormalisedSigDecl::Val {
                attributes,
                name,
                access,
                typars,
                constraints,
                ty,
                literal,
            }
        }
        "Types" => {
            // `SynModuleSigDecl.Types(types: SynTypeDefnSig list, range)` (phase
            // 10.14, first slice). Field 0 is the list of definitions (one per
            // group; only `and` joins several — a later slice); range elided.
            let f = fields(v);
            let defns = f[0]
                .as_array()
                .expect("SynModuleSigDecl.Types field 0 (types) must be array")
                .iter()
                .map(fcs_sig_type_defn)
                .collect();
            NormalisedSigDecl::Types(defns)
        }
        "Exception" => {
            // `SynModuleSigDecl.Exception(exnSig: SynExceptionSig, range)` (phase
            // 10.15). Field 0 is the `SynExceptionSig(exnRepr, withKeyword,
            // members, range)`, whose field layout matches the impl
            // `SynExceptionDefn` (repr at 0, members at 2) — so the shared
            // `fcs_exception_defn` reads it directly. `sig = true` maps the
            // `with member …` augmentation members as member *sigs*
            // (`SynMemberSig` via `fcs_member_sig`).
            let f = fields(v);
            NormalisedSigDecl::Exception(fcs_exception_defn(&f[0], true))
        }
        "HashDirective" => {
            // `SynModuleSigDecl.HashDirective(ParsedHashDirective, range)`.
            // Field 0 is the shared `ParsedHashDirective` payload.
            let (ident, args) = fcs_hash_directive_payload(&fields(v)[0]);
            NormalisedSigDecl::HashDirective { ident, args }
        }
        other => panic!("Phase 10.15: unsupported SynModuleSigDecl case {other:?}"),
    }
}

/// Project one `SynTypeDefnSig(typeInfo, typeRepr, members, range, trivia)`
/// (phase 10.14) to the shared [`NormalisedTypeDefn`]. The `SynComponentInfo`
/// (field 0) is read exactly like the impl-side [`fcs_type_defn`]: field 0 its
/// `attributes`, field 1 `typeParams`, field 2 `constraints`, field 3 `longId`.
/// The repr (field 1) is a `SynTypeDefnSigRepr` whose `Simple` case wraps the
/// same `SynTypeDefnSimpleRepr` as the impl side (so [`fcs_simple_repr`] is
/// reused). Unlike `SynTypeDefn`, a `SynTypeDefnSig` has **no**
/// implicit-constructor slot, so `implicit_ctor` is always `None`; the outer
/// `members` list (field 2, `SynMemberSig list`) carries a `with`-augmentation's
/// member sigs (phase 10.14 slice 4), projected through [`fcs_member_sig`].
fn fcs_sig_type_defn(v: &Value) -> NormalisedTypeDefn {
    let f = fields(v);
    let component_info = fields(&f[0]);
    let attributes = fcs_attribute_lists(&component_info[0]);
    let typars = fcs_typar_decls(&component_info[1]);
    let constraints = fcs_type_defn_constraints(&component_info[1], &component_info[2]);
    let long_id = fcs_ident_list_texts(&component_info[3]);
    // `SynComponentInfo.accessibility` (field 6) — the type header's own access.
    let access = fcs_access(&component_info[6]);
    let repr = fcs_sig_type_repr(&f[1]);
    // The *outer* `members: SynMemberSig list` (field 2) — a `with`-augmentation's
    // member sigs (`type T with member M : int`, phase 10.14 slice 4) or trailing
    // members on a structural repr. FCS homes them here (mirroring the impl-side
    // `SynTypeDefn.members`), distinct from a pure object model's members, which
    // nest inside the `ObjectModel` repr. Project each through `fcs_member_sig`,
    // exactly as the impl side reads `SynTypeDefn.members`.
    let members = f[2]
        .as_array()
        .expect("SynTypeDefnSig field 2 (members) must be array")
        .iter()
        .map(fcs_member_sig)
        .collect();
    NormalisedTypeDefn {
        attributes,
        access,
        long_id,
        typars,
        constraints,
        repr,
        members,
        implicit_ctor: None,
    }
}

/// Project a `SynTypeDefnSigRepr` (phase 10.14). `Simple(repr, range)` (field 0)
/// shares the impl-side `SynTypeDefnSimpleRepr` via [`fcs_simple_repr`] (the
/// abbreviation / opaque / record / union / enum forms). `ObjectModel(kind,
/// memberSigs, range)` (slice 3a) projects to the shared
/// [`NormalisedTypeRepr::ObjectModel`]: field 0 the `SynTypeDefnKind` (via
/// [`fcs_type_defn_kind`]), field 1 the `SynMemberSig list` (via
/// [`fcs_member_sig`]). A `delegate of …` (slice 7) is lowered to
/// `ObjectModel(Delegate(ty, arity), [Invoke], _)`; keep only the signature `ty`
/// (field 0 of the kind), exactly as the impl-side [`fcs_type_repr`] — the `arity`
/// and the synthetic `Invoke` slot are both derived from it. The `Exception` case
/// `panic!`s until its slice lands.
fn fcs_sig_type_repr(v: &Value) -> NormalisedTypeRepr {
    match case_name(v) {
        "Simple" => fcs_simple_repr(&fields(v)[0]),
        "ObjectModel" => {
            let f = fields(v);
            if case_name(&f[0]) == "Delegate" {
                let kf = fields(&f[0]);
                return NormalisedTypeRepr::Delegate(fcs_type(&kf[0]));
            }
            let kind = fcs_type_defn_kind(&f[0]);
            let members = f[1]
                .as_array()
                .expect("SynTypeDefnSigRepr.ObjectModel field 1 (memberSigs) must be array")
                .iter()
                .map(fcs_member_sig)
                .collect();
            NormalisedTypeRepr::ObjectModel { kind, members }
        }
        other => panic!("Phase 10.14: unsupported SynTypeDefnSigRepr case {other:?}"),
    }
}

/// Project one `SynMemberSig`. The shared normalised forms ([`NormalisedMember`])
/// elide the `SynMemberSig`-vs-`SynMemberDefn` distinction, so each sig variant
/// maps to the same target as its impl counterpart:
/// * `Member(memberSig: SynValSig, flags, …)` (slice 3a) — `member`/`abstract`/
///   `static member` — to [`NormalisedMember::AbstractSlot`] (name + type via
///   [`fcs_val_sig`]; the leading keyword from the `SynValSig` trivia field 11,
///   exactly as the impl-side abstract-slot arm of [`fcs_member`]);
/// * `Inherit(inheritedType: SynType, range)` (slice 3b) — `inherit T` — to
///   [`NormalisedMember::Inherit`] (the type is non-optional, field 0);
/// * `Interface(interfaceType: SynType, range)` (slice 3b) — `interface I` — to
///   [`NormalisedMember::Interface`] (a sig interface has no members → `None`);
/// * `ValField(field: SynField, range)` (slice 3b) — `val x : T` — to
///   [`NormalisedMember::ValField`] via [`fcs_field`].
///
/// `NestedType` `panic!`s until its slice lands.
fn fcs_member_sig(v: &Value) -> NormalisedMember {
    match case_name(v) {
        "Member" => {
            let slot = &fields(v)[0];
            let (name, ty) = fcs_val_sig(slot);
            let leading_keyword = fcs_leading_keyword(
                fields(slot)[11]
                    .get("LeadingKeyword")
                    .expect("SynValSig trivia (field 11) must have LeadingKeyword"),
            );
            let attributes = fcs_attribute_lists(&fields(slot)[0]);
            // The `= <literal>` value (phase 10.12 member-literal) —
            // `SynValSig.synExpr` (field 9, a `SynExpr option`), projected via the
            // shared expression normaliser, exactly as the module-level val sig.
            let literal = match &fields(slot)[9] {
                Value::Null => None,
                some => Some(Box::new(fcs_expr(&fields(some)[0]))),
            };
            NormalisedMember::AbstractSlot {
                name,
                ty,
                leading_keyword,
                attributes,
                literal,
                // `SynValSig.accessibility` (field 8, a `SynValSigAccess`).
                access: fcs_val_sig_access(&fields(slot)[8]),
            }
        }
        "Inherit" => NormalisedMember::Inherit {
            base_type: Some(fcs_type(&fields(v)[0])),
        },
        "Interface" => NormalisedMember::Interface {
            interface_type: fcs_type(&fields(v)[0]),
            members: None,
        },
        "ValField" => NormalisedMember::ValField(fcs_field(&fields(v)[0])),
        other => panic!("Phase 10.14: unsupported SynMemberSig case {other:?}"),
    }
}

fn fcs_impl_file(v: &Value) -> NormalisedImplFile {
    // `ParsedImplFileInput` is a record DU case with positional fields:
    //   0: filename (string)
    //   1: isScript (bool)
    //   2: QualifiedNameOfFile
    //   3: scoped pragmas
    //   4: list of SynModuleOrNamespace            <-- what we want
    //   5: isLastCompiland tuple
    //   6: trivia
    //   7: identifiers
    let fields = fields(v);
    let warn_directives = fcs_warn_directives(&fields[6]);
    let modules = fields[4]
        .as_array()
        .expect("ParsedImplFileInput.modules must be array");
    NormalisedImplFile {
        warn_directives,
        modules: modules.iter().map(fcs_module).collect(),
    }
}

fn fcs_warn_directives(trivia: &Value) -> Vec<NormalisedWarnDirectiveKind> {
    let Some(items) = trivia.get("WarnDirectives").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|v| match case_name(v) {
            "Nowarn" => NormalisedWarnDirectiveKind::Nowarn,
            "Warnon" => NormalisedWarnDirectiveKind::Warnon,
            other => panic!("unsupported warning directive case {other:?}"),
        })
        .collect()
}

/// The shared header projection for `SynModuleOrNamespace` (impl) and
/// `SynModuleOrNamespaceSig` (sig) — the two DUs have identical leading fields:
/// 0 `longId` (`Ident list`), 1 `isRecursive`, 2 the `SynModuleOrNamespaceKind`
/// (`AnonModule` / `NamedModule` / `DeclaredNamespace` / `GlobalNamespace`),
/// 3 the decls list, 4 `xmlDoc`, 5 `attribs`, 6 `accessibility`. Returns
/// `(kind, isRecursive, attributes, access)`; the caller projects field 3
/// (decls) with its own decl projector.
fn fcs_module_header(
    v: &Value,
) -> (
    NormalisedModuleKind,
    bool,
    Vec<Vec<NormalisedAttribute>>,
    Option<NormalisedAccess>,
) {
    let fields = fields(v);
    let is_rec = fields[1]
        .as_bool()
        .expect("SynModuleOrNamespace field 1 (isRecursive) must be a JSON bool");
    // `attribs` (field 5) — the whole-file `[<AutoOpen>] module Foo` header
    // attributes (phase 10.7e). Empty for an anonymous module / a namespace.
    let attributes = fcs_attribute_lists(&fields[5]);
    // `accessibility` (field 6) — `module internal M`.
    let access = fcs_access(&fields[6]);
    let kind = match case_name(&fields[2]) {
        // AnonModule's `longId` is filename-derived (random under tempfiles),
        // so it is deliberately not projected.
        "AnonModule" => NormalisedModuleKind::Anon,
        "NamedModule" => NormalisedModuleKind::Named {
            long_id: fcs_ident_list_texts(&fields[0]),
            kind: NamedKind::Module,
        },
        "DeclaredNamespace" => NormalisedModuleKind::Named {
            long_id: fcs_ident_list_texts(&fields[0]),
            kind: NamedKind::Namespace,
        },
        // GlobalNamespace's `longId` is already `[]` (the post-parse pass
        // strips the leading `global`); project it as empty regardless.
        "GlobalNamespace" => NormalisedModuleKind::Named {
            long_id: Vec::new(),
            kind: NamedKind::GlobalNamespace,
        },
        other => panic!("unsupported module kind {other:?}"),
    };
    (kind, is_rec, attributes, access)
}

fn fcs_module(v: &Value) -> NormalisedModule {
    let (kind, is_rec, attributes, access) = fcs_module_header(v);
    let decls = fields(v)[3]
        .as_array()
        .expect("SynModuleOrNamespace.decls must be array");
    NormalisedModule {
        kind,
        is_rec,
        attributes,
        access,
        decls: decls.iter().map(fcs_decl).collect(),
    }
}

/// Extract the segment texts of an `Ident list` — a plain list of `Ident`
/// records (`{ idText, idRange }`), as carried by
/// `SynModuleOrNamespace.longId`. Distinct from
/// [`fcs_syn_long_ident_segments`], which projects a `SynLongIdent` DU
/// (ident list + dot ranges + operator trivia); a module/namespace header
/// name has no operator-notation trivia, so the bare `idText` suffices.
fn fcs_ident_list_texts(idents: &Value) -> Vec<String> {
    idents
        .as_array()
        .expect("SynModuleOrNamespace.longId must be an array")
        .iter()
        .map(|id| {
            let t = id
                .get("idText")
                .and_then(Value::as_str)
                .expect("Ident record has idText");
            // FCS spells a `global` path head as the single-backtick-quoted
            // `` `global` `` (a keyword reused as an identifier); our parser
            // emits the bare `global` text, so strip a surrounding single
            // backtick pair to line both sides up. A double-backtick-quoted
            // user identifier (`` ``My Mod`` ``) already has clean `idText`, so
            // this is a no-op there.
            t.strip_prefix('`')
                .and_then(|s| s.strip_suffix('`'))
                .unwrap_or(t)
                .to_string()
        })
        .collect()
}

/// Extract the segment texts of a `SynLongIdent` (the DU case `SynLongIdent
/// of id: Ident list * dotRanges: range list * trivia: IdentTrivia option
/// list`). Field 0 is the ident list, field 2 the parallel trivia list whose
/// `OriginalNotation` slot — when present — holds the source spelling FCS
/// rewrote away (`mkSynOperator` mangles `+` to `op_Addition` and stashes
/// `Some (IdentTrivia.OriginalNotation "+")`), so we prefer it over `idText`
/// to round-trip operator idents. Shared by every `SynLongIdent` projection
/// (expr / type / pat / open target).
fn fcs_syn_long_ident_segments(syn_long_ident: &Value) -> Vec<String> {
    let li_fields = fields(syn_long_ident);
    let idents = li_fields[0]
        .as_array()
        .expect("SynLongIdent ident list must be array");
    let trivia = li_fields[2]
        .as_array()
        .expect("SynLongIdent trivia list must be array");
    idents
        .iter()
        .enumerate()
        .map(|(i, id)| {
            if let Some(tv) = trivia.get(i)
                && let Some(text) = ident_original_notation(tv)
            {
                return text;
            }
            let t = id
                .get("idText")
                .and_then(Value::as_str)
                .expect("Ident record has idText");
            // FCS spells a `global` path head as the single-backtick-quoted
            // `` `global` `` (a keyword reused as an identifier); our parser
            // emits the bare `global` text, so strip a surrounding single
            // backtick pair to line both sides up. A double-backtick-quoted
            // user identifier (`` ``My Mod`` ``) already has clean `idText`
            // (the lexer's backtick-normalisation removed the quotes), so this
            // single-pair strip is a no-op there. Mirrors the same strip in
            // [`fcs_ident_list_texts`] for the module/namespace-header path.
            t.strip_prefix('`')
                .and_then(|s| s.strip_suffix('`'))
                .unwrap_or(t)
                .to_string()
        })
        .collect()
}

fn fcs_decl(v: &Value) -> NormalisedDecl {
    let case = case_name(v);
    match case {
        "Expr" => {
            // `SynModuleDecl.Expr of expr * range`. Field 0 is the SynExpr.
            let fields = fields(v);
            NormalisedDecl::Expr(fcs_expr(&fields[0]))
        }
        "Let" => {
            // `SynModuleDecl.Let(isRec, bindings, range, trivia)` — fields
            // 0 (isRec) and 1 (bindings list) carry the binding semantics;
            // range / trivia are elided.
            let fields = fields(v);
            let is_rec = fields[0]
                .as_bool()
                .expect("SynModuleDecl.Let field 0 (isRec) must be a JSON bool");
            let bindings = fields[1]
                .as_array()
                .expect("SynModuleDecl.Let field 1 (bindings) must be array")
                .iter()
                .map(fcs_binding)
                .collect();
            NormalisedDecl::Let { is_rec, bindings }
        }
        "Open" => {
            // `SynModuleDecl.Open(target: SynOpenDeclTarget, range)`. Field 0
            // is the target DU; range is elided.
            let f = fields(v);
            let target = &f[0];
            let normalised = match case_name(target) {
                "ModuleOrNamespace" => {
                    // `ModuleOrNamespace(longId: SynLongIdent, range)`.
                    let tf = fields(target);
                    NormalisedOpenTarget::ModuleOrNamespace(fcs_syn_long_ident_segments(&tf[0]))
                }
                "Type" => {
                    // `Type(typeName: SynType, range)`.
                    let tf = fields(target);
                    NormalisedOpenTarget::Type(fcs_type(&tf[0]))
                }
                other => panic!("unknown SynOpenDeclTarget case {other:?}"),
            };
            NormalisedDecl::Open { target: normalised }
        }
        "NestedModule" => {
            // `SynModuleDecl.NestedModule(moduleInfo: SynComponentInfo,
            // isRecursive, decls, isContinuing, range, trivia)`. The name is
            // `SynComponentInfo.longId` (field 3, a plain `Ident list`); the
            // header attributes are `SynComponentInfo.attributes` (field 0, phase
            // 10.7d); the accessibility is field 6 (`module internal N =`).
            // typars / constraints / xmlDoc / `isContinuing` / ranges are elided.
            let f = fields(v);
            let module_info = fields(&f[0]);
            let attributes = fcs_attribute_lists(&module_info[0]);
            let long_id = fcs_ident_list_texts(&module_info[3]);
            // `SynComponentInfo.accessibility` (field 6) — `module internal N =`.
            let access = fcs_access(&module_info[6]);
            let is_rec = f[1]
                .as_bool()
                .expect("SynModuleDecl.NestedModule field 1 (isRecursive) must be a JSON bool");
            let decls = f[2]
                .as_array()
                .expect("SynModuleDecl.NestedModule field 2 (decls) must be array")
                .iter()
                .map(fcs_decl)
                .collect();
            NormalisedDecl::NestedModule {
                long_id,
                is_rec,
                access,
                attributes,
                decls,
            }
        }
        "ModuleAbbrev" => {
            // `SynModuleDecl.ModuleAbbrev(ident: Ident, longId: LongIdent,
            // range)`. Field 0 is the single LHS `Ident`; field 1 the RHS
            // `LongIdent` (a plain `Ident list`); range is elided.
            let f = fields(v);
            let ident = f[0]
                .get("idText")
                .and_then(Value::as_str)
                .expect("SynModuleDecl.ModuleAbbrev field 0 (ident) has idText")
                .to_string();
            NormalisedDecl::ModuleAbbrev {
                ident,
                long_id: fcs_ident_list_texts(&f[1]),
            }
        }
        "Types" => {
            // `SynModuleDecl.Types(typeDefns: SynTypeDefn list, range)`. Field
            // 0 is the list of definitions (one per group; only `and` joins
            // several); range is elided.
            let f = fields(v);
            let defns = f[0]
                .as_array()
                .expect("SynModuleDecl.Types field 0 (typeDefns) must be array")
                .iter()
                .map(fcs_type_defn)
                .collect();
            NormalisedDecl::Types(defns)
        }
        "Exception" => {
            // `SynModuleDecl.Exception(exnDefn: SynExceptionDefn, range)`. Field
            // 0 is the `SynExceptionDefn`; range is elided.
            let f = fields(v);
            NormalisedDecl::Exception(fcs_exception_defn(&f[0], false))
        }
        "Attributes" => {
            // `SynModuleDecl.Attributes(attributes: SynAttributes, range)` (phase
            // 10.7). Field 0 is the `SynAttributeList list`; range elided.
            let f = fields(v);
            NormalisedDecl::Attributes(fcs_attribute_lists(&f[0]))
        }
        "HashDirective" => {
            // `SynModuleDecl.HashDirective(ParsedHashDirective, range)`. Field 0 is
            // the `ParsedHashDirective(ident, args, range)`; range elided.
            let (ident, args) = fcs_hash_directive_payload(&fields(v)[0]);
            NormalisedDecl::HashDirective { ident, args }
        }
        other => panic!("Phase 1: unsupported SynModuleDecl case {other:?}"),
    }
}

fn fcs_hash_directive_payload(v: &Value) -> (String, Vec<NormalisedHashDirectiveArg>) {
    let hd = fields(v);
    let ident = hd[0]
        .as_str()
        .expect("ParsedHashDirective ident (field 0) must be a string")
        .to_string();
    let args = hd[1]
        .as_array()
        .expect("ParsedHashDirective args (field 1) must be an array")
        .iter()
        .map(fcs_hash_directive_arg)
        .collect();
    (ident, args)
}

/// Project one `ParsedHashDirectiveArgument`. The `SourceIdentifier`'s resolved
/// `value` (field 1) is validated against its range before path-valued source
/// identifiers are canonicalised.
fn fcs_hash_directive_arg(v: &Value) -> NormalisedHashDirectiveArg {
    match case_name(v) {
        "String" => {
            let f = fields(v);
            NormalisedHashDirectiveArg::String {
                value: fcs_utf16_units(&f[0], "ParsedHashDirectiveArgument.String field 0 (value)"),
                kind: fcs_syn_string_kind(&f[1]),
            }
        }
        "Int32" => {
            let f = fields(v);
            NormalisedHashDirectiveArg::Int32(
                f[0].as_i64()
                    .expect("ParsedHashDirectiveArgument.Int32 value must be an integer")
                    as i32,
            )
        }
        "Ident" => {
            // `Ident(ident: Ident, range)` — field 0 is an `Ident` record.
            let f = fields(v);
            NormalisedHashDirectiveArg::Ident(
                f[0].get("idText")
                    .and_then(Value::as_str)
                    .expect("ParsedHashDirectiveArgument.Ident field 0 must be an Ident record")
                    .to_string(),
            )
        }
        "SourceIdentifier" => {
            let f = fields(v);
            let ident = f[0]
                .as_str()
                .expect("ParsedHashDirectiveArgument.SourceIdentifier ident must be a string")
                .to_string();
            let value = fcs_source_identifier_value(
                &ident,
                &f[1],
                &f[2],
                "ParsedHashDirectiveArgument.SourceIdentifier value",
            );
            NormalisedHashDirectiveArg::SourceIdentifier { ident, value }
        }
        other => panic!("unsupported ParsedHashDirectiveArgument case {other:?}"),
    }
}

/// Project a `SynExceptionDefn(exnRepr: SynExceptionDefnRepr, withKeyword,
/// members, range)` (phase 9.15a). The repr (field 0) is
/// `SynExceptionDefnRepr(attributes, caseName: SynUnionCase, longId: LongIdent
/// option, xmlDoc, accessibility, range)`: field 0 is the attribute lists (phase
/// 10.7m), field 1 the reused union case (projected via [`fcs_union_case`]),
/// field 2 the abbreviation target, field 4 the `accessibility`
/// (`exception private E`). The `SynExceptionDefn`'s field 2 is the
/// augmentation members (phase 9.15b); `withKeyword` / xmlDoc / ranges are
/// elided.
///
/// `sig` selects the augmentation member kind: `false` for the impl exception
/// (member *bodies*, `SynMemberDefn` via [`fcs_member`]); `true` for the
/// signature exception (`SynExceptionSig`, phase 10.15) whose `members` are
/// member *sigs* (`SynMemberSig` via [`fcs_member_sig`]). `SynExceptionSig` shares
/// `SynExceptionDefn`'s field layout (repr 0, members 2), so the same projector
/// reads both.
fn fcs_exception_defn(v: &Value, sig: bool) -> NormalisedExnDefn {
    let defn = fields(v);
    let repr = fields(&defn[0]);
    // `SynExceptionDefnRepr` field 0 is `attributes: SynAttributes` (phase
    // 10.7m) — FCS concatenates the leading `[<A>] exception …` lists with any
    // after-keyword `exception [<B>] …` lists into this one field.
    let attributes = fcs_attribute_lists(&repr[0]);
    let case = fcs_union_case(&repr[1]);
    // `SynExceptionDefnRepr.accessibility` (field 4) — `exception private E`.
    let access = fcs_access(&repr[4]);
    // `longId: LongIdent option` (field 2) — `null` (None) or
    // `{Case:"Some", Fields:[ident list]}`.
    let abbrev = if repr[2].is_null() {
        None
    } else {
        match case_name(&repr[2]) {
            "Some" => Some(fcs_ident_list_texts(&fields(&repr[2])[0])),
            "None" => None,
            other => panic!("unexpected SynExceptionDefnRepr longId Option case {other:?}"),
        }
    };
    let members = defn[2]
        .as_array()
        .expect("SynExceptionDefn field 2 (members) must be array")
        .iter()
        .map(if sig { fcs_member_sig } else { fcs_member })
        .collect();
    NormalisedExnDefn {
        attributes,
        access,
        case,
        abbrev,
        members,
    }
}

/// Project one `SynTypeDefn(typeInfo, typeRepr, members, implicitConstructor,
/// range, trivia)` (phase 9.1/9.3). `SynComponentInfo` is field 0: its field 1
/// is `typeParams` (`SynTyparDecls option`, phase 9.3) and field 3 is `longId`
/// (a plain `Ident list`); field 0 is `attributes` (phase 10.7a), field 6 the
/// header `accessibility`. The repr is field 1. `preferPostfix` / xmlDoc /
/// trivia are elided.
fn fcs_type_defn(v: &Value) -> NormalisedTypeDefn {
    let f = fields(v);
    let component_info = fields(&f[0]);
    // `SynComponentInfo` field 0 is `attributes: SynAttributes` (phase 10.7a).
    let attributes = fcs_attribute_lists(&component_info[0]);
    let typars = fcs_typar_decls(&component_info[1]);
    let constraints = fcs_type_defn_constraints(&component_info[1], &component_info[2]);
    let long_id = fcs_ident_list_texts(&component_info[3]);
    // `SynComponentInfo.accessibility` (field 6) — `type internal Foo`.
    let access = fcs_access(&component_info[6]);
    let repr = fcs_type_repr(&f[1]);
    // `SynTypeDefn(typeInfo, typeRepr, members, implicitConstructor, …)` — field
    // 2 is the *outer* member list (phase 9.13): an augmentation's members or
    // trailing members on a simple repr. Empty for a pure object model (FCS puts
    // those in the repr, not here) and for the implicit ctor (its own slot).
    let members = f[2]
        .as_array()
        .expect("SynTypeDefn field 2 (members) must be array")
        .iter()
        .map(fcs_member)
        .collect();
    // Field 3 is `implicitConstructor: SynMemberDefn option` (phase 9.8a). The
    // same ctor is also `ObjectModel.members[0]` (FCS prepends it), which
    // `fcs_type_repr` already projects; here we read the dedicated slot.
    let implicit_ctor = if f[3].is_null() {
        None
    } else {
        match case_name(&f[3]) {
            "Some" => Some(fcs_member(&fields(&f[3])[0])),
            "None" => None,
            other => panic!("unexpected SynTypeDefn.implicitConstructor Option case {other:?}"),
        }
    };
    NormalisedTypeDefn {
        attributes,
        access,
        long_id,
        typars,
        constraints,
        repr,
        members,
        implicit_ctor,
    }
}

/// Collect a definition's type-parameter constraints (phase 9.3b) from both
/// FCS sources, in source order: the inside-`<>` `when` clause lives in
/// `SynTyparDecls.PostfixList`'s constraints (field 1 of the `PostfixList`,
/// reachable through `SynComponentInfo.typeParams`), and the after-decls clause
/// in `SynComponentInfo.constraints` directly. `PrefixList`/`SinglePrefix`
/// carry no constraints.
fn fcs_type_defn_constraints(
    typeparams: &Value,
    ci_constraints: &Value,
) -> Vec<NormalisedTypeConstraint> {
    let mut out = Vec::new();
    if !typeparams.is_null() && case_name(typeparams) == "Some" {
        let inner = &fields(typeparams)[0];
        if case_name(inner) == "PostfixList"
            && let Some(arr) = fields(inner)[1].as_array()
        {
            out.extend(arr.iter().map(fcs_type_constraint));
        }
    }
    if let Some(arr) = ci_constraints.as_array() {
        out.extend(arr.iter().map(fcs_type_constraint));
    }
    out
}

/// Project one `SynTypeConstraint` (phase 9.3b) to the subject typar (and, for
/// the subtype form, the constraint type). Most variants' field 0 is the
/// `SynTypar` (the SRTP-member and self-constrained forms carry a `SynType`
/// there instead). The out-of-scope `default` variant `panic!`s — it has no
/// parser surface, so it cannot appear in a clean diff and a stray one should
/// fail loudly.
/// Project a `SynType list` (e.g. the `enum`/`delegate` constraint type-argument
/// list) to normalised types.
fn fcs_type_arg_list(v: &Value) -> Vec<NormalisedType> {
    v.as_array()
        .expect("constraint type-argument list must be an array")
        .iter()
        .map(fcs_type)
        .collect()
}

/// Flatten the support type of a `WhereTyparSupportsMember` constraint (or a
/// `SynExpr.TraitCall`) to its alternative list. The `Paren`/`Or` structure of a
/// `(^a or ^b)` / `(Witnesses or ^T)` support is elided: `SynType.Paren(inner)`
/// → the inner's alternatives; `SynType.Or(lhs, rhs)` → `lhs` then `rhs`
/// (left-associative `typeAlts`, in source order). Any *other* support shape is
/// a leaf alternative projected via [`fcs_type`] — a single typar `^T` (a
/// `SynType.Var`), or a concrete `appTypeWithoutNull` (`Witnesses`, a
/// `SynType.LongIdent`; `IParsable<int>`, a `SynType.App`) in the general-type
/// alternatives form.
fn fcs_support_types(v: &Value) -> Vec<NormalisedType> {
    match case_name(v) {
        // The *outer* `Paren` wrapping the `(… or …)` alternatives list is
        // structural (the parens of `(^a or ^b)`) and elided; a single-typar
        // support `^T` has no such wrapper and falls to the leaf arm.
        "Paren" => fcs_flatten_or_alts(&fields(v)[0]),
        _ => vec![fcs_type(v)],
    }
}

/// Flatten the `Or`-tree *inside* a support's outer parens to its leaf
/// alternatives, in source order (left-associative `typeAlts`). Only the `Or`
/// spine is structural; each leaf alternative is projected via [`fcs_type`],
/// which **preserves** its own shape — a parenthesised operand `((IFoo) or ^T)`
/// keeps its inner `SynType.Paren` (matching the CST's `PAREN_TYPE`), rather
/// than being flattened like the outer wrapper.
fn fcs_flatten_or_alts(v: &Value) -> Vec<NormalisedType> {
    match case_name(v) {
        "Or" => {
            let f = fields(v);
            let mut types = fcs_flatten_or_alts(&f[0]);
            types.extend(fcs_flatten_or_alts(&f[1]));
            types
        }
        _ => vec![fcs_type(v)],
    }
}

fn fcs_type_constraint(v: &Value) -> NormalisedTypeConstraint {
    let f = fields(v);
    match case_name(v) {
        "WhereTyparSubtypeOfType" => NormalisedTypeConstraint::SubtypeOf {
            typar: fcs_syntypar(&f[0]),
            ty: fcs_type(&f[1]),
        },
        "WhereTyparIsValueType" => NormalisedTypeConstraint::IsValueType(fcs_syntypar(&f[0])),
        "WhereTyparIsReferenceType" => {
            NormalisedTypeConstraint::IsReferenceType(fcs_syntypar(&f[0]))
        }
        "WhereTyparSupportsNull" => NormalisedTypeConstraint::SupportsNull(fcs_syntypar(&f[0])),
        "WhereTyparNotSupportsNull" => {
            NormalisedTypeConstraint::NotSupportsNull(fcs_syntypar(&f[0]))
        }
        "WhereTyparIsComparable" => NormalisedTypeConstraint::IsComparable(fcs_syntypar(&f[0])),
        "WhereTyparIsEquatable" => NormalisedTypeConstraint::IsEquatable(fcs_syntypar(&f[0])),
        "WhereTyparIsUnmanaged" => NormalisedTypeConstraint::IsUnmanaged(fcs_syntypar(&f[0])),
        // `^T : (static member M : sig)` (SRTP). Field 0 is the support type — a
        // `SynType.Var` for a single typar, or `SynType.Paren(SynType.Or(…))` for
        // the parenthesised alternatives form `(^a or ^b) : (…)` /
        // `(Witnesses or ^T) : (…)`, flattened to the alternative type list by
        // `fcs_support_types`; field 1 is the `SynMemberSig`, shared with the
        // signature member-sig projection.
        "WhereTyparSupportsMember" => NormalisedTypeConstraint::SupportsMember {
            support: fcs_support_types(&f[0]),
            member: Box::new(fcs_member_sig(&f[1])),
        },
        // `'a : enum<'b>` / `'a : delegate<args, ret>` — field 0 is the subject
        // `SynTypar`, field 1 the `SynType list` of `< … >` type arguments.
        "WhereTyparIsEnum" => NormalisedTypeConstraint::IsEnum {
            typar: fcs_syntypar(&f[0]),
            args: fcs_type_arg_list(&f[1]),
        },
        "WhereTyparIsDelegate" => NormalisedTypeConstraint::IsDelegate {
            typar: fcs_syntypar(&f[0]),
            args: fcs_type_arg_list(&f[1]),
        },
        // `when IFoo<'T>` (F# 7 IWSAM shorthand) — field 0 is the constraint
        // type (an ordinary `SynType`, no subject typar); field 1 the range.
        "WhereSelfConstrained" => NormalisedTypeConstraint::SelfConstrained(fcs_type(&f[0])),
        other => {
            panic!("unsupported SynTypeConstraint case {other:?} (phase 9.3b covers a subset)")
        }
    }
}

/// Project `SynComponentInfo.typeParams` (`SynTyparDecls option`, phase 9.3) to
/// the flat typar list. `null` → empty; otherwise the `PostfixList`/`PrefixList`
/// (field 0 = `SynTyparDecl list`) or `SinglePrefix` (field 0 = one
/// `SynTyparDecl`) cases. The variant and `preferPostfix` are elided.
fn fcs_typar_decls(v: &Value) -> Vec<NormalisedTypar> {
    // `option` is serialised as `null` (None) or `{Case:"Some", Fields:[inner]}`.
    if v.is_null() {
        return Vec::new();
    }
    let inner = match case_name(v) {
        "Some" => &fields(v)[0],
        "None" => return Vec::new(),
        other => panic!("unexpected SynComponentInfo.typeParams Option case {other:?}"),
    };
    let f = fields(inner);
    match case_name(inner) {
        "PostfixList" | "PrefixList" => f[0]
            .as_array()
            .expect("SynTyparDecls list field must be array")
            .iter()
            .map(fcs_typar)
            .collect(),
        "SinglePrefix" => vec![fcs_typar(&f[0])],
        other => panic!("unknown SynTyparDecls case {other:?}"),
    }
}

/// Project `SynPat.LongIdent.typars` (`SynValTyparDecls option`, field 2) to the
/// flat typar list. `null` (None) → empty. Otherwise unwrap the `Some` to the
/// `SynValTyparDecls(typarDecls: SynTyparDecls option, canInfer)` record and
/// reuse [`fcs_typar_decls`] on its inner `SynTyparDecls option` (field 0).
/// FCS's synthetic `noInferredTypars` ctor-head marker has `typarDecls = None`,
/// so it projects to empty — matching our side, which emits no `TYPAR_DECLS`
/// node for a non-generic head. `canInfer` is elided.
fn fcs_pat_typar_decls(v: &Value) -> Vec<NormalisedTypar> {
    if v.is_null() {
        return Vec::new();
    }
    let syn_val_typar_decls = match case_name(v) {
        "Some" => &fields(v)[0],
        "None" => return Vec::new(),
        other => panic!("unexpected SynPat.LongIdent typars Option case {other:?}"),
    };
    fcs_typar_decls(&fields(syn_val_typar_decls)[0])
}

/// Project one `SynTyparDecl(attributes, typar: SynTypar, intersection, trivia)`
/// to its typar. Field 0 is the `attributes: SynAttributes` (`type T<[<Measure>]
/// 'a>`); field 1 is the `SynTypar(ident, staticReq, _)`, whose static-req
/// discriminant is read for `^a` (`HeadType`) vs `'a` (`None`). Intersection
/// constraints are a later slice, elided.
fn fcs_typar(v: &Value) -> NormalisedTypar {
    let f = fields(v);
    NormalisedTypar {
        attributes: fcs_attribute_lists(&f[0]),
        // `SynTyparDecl.intersectionConstraints` (field 2) — the `& #seq<int>`
        // flexible-type run, each a `SynType` (via `fcs_type`). `null`/`[]` when
        // the typar carries no constraints.
        intersection_constraints: f[2]
            .as_array()
            .map(|a| a.iter().map(fcs_type).collect())
            .unwrap_or_default(),
        ..fcs_syntypar(&f[1])
    }
}

/// Project a `SynTypar(ident, staticReq, isCompGen)` to its name and head-type
/// flag (with empty attributes — a bare `SynTypar` has no `SynTyparDecl`
/// wrapper). Used directly by the constraint projector (whose subject is a bare
/// `SynTypar`) and via [`fcs_typar`] for a `SynTyparDecl` (which overlays the
/// declaration's attributes).
fn fcs_syntypar(v: &Value) -> NormalisedTypar {
    let typar_fields = fields(v);
    let name = typar_fields[0]
        .get("idText")
        .and_then(Value::as_str)
        .expect("SynTypar Ident record has idText")
        .to_string();
    let head_type = match case_name(&typar_fields[1]) {
        "None" => false,
        "HeadType" => true,
        other => panic!("unknown TyparStaticReq case {other:?}"),
    };
    NormalisedTypar {
        name,
        head_type,
        attributes: Vec::new(),
        // A bare `SynTypar` has no `SynTyparDecl` wrapper, so no intersection
        // constraints; `fcs_typar` overlays field 2 for the declaration form.
        intersection_constraints: Vec::new(),
    }
}

/// Project one `SynStaticOptimizationConstraint` (`SyntaxTree.fsi:1048`) — a
/// condition of a [`NormalisedExpr::StaticOptimization`] clause. The subject is a
/// bare `SynTypar` (via [`fcs_syntypar`], no `SynTyparDecl` attribute wrapper).
fn fcs_static_opt_constraint(v: &Value) -> NormalisedStaticOptConstraint {
    match case_name(v) {
        "WhenTyparTyconEqualsTycon" => {
            // `WhenTyparTyconEqualsTycon(typar: SynTypar, rhsType: SynType, range)`.
            let f = fields(v);
            NormalisedStaticOptConstraint::WhenTyparTyconEqualsTycon {
                typar: fcs_syntypar(&f[0]),
                rhs_type: fcs_type(&f[1]),
            }
        }
        "WhenTyparIsStruct" => {
            // `WhenTyparIsStruct(typar: SynTypar, range)` — the bare `'T struct`.
            let f = fields(v);
            NormalisedStaticOptConstraint::WhenTyparIsStruct {
                typar: fcs_syntypar(&f[0]),
            }
        }
        other => panic!("unknown SynStaticOptimizationConstraint case {other:?}"),
    }
}

/// Project a `SynTypeDefnRepr` — `Simple(SynTypeDefnSimpleRepr, range)` (the
/// `SynTypeDefnSimpleRepr` is projected by the shared [`fcs_simple_repr`]) or an
/// `ObjectModel(kind, members, range)`. Other (`Exception`) reprs `panic!` until
/// their slices land, so an out-of-scope shape fails loudly rather than
/// projecting wrong.
fn fcs_type_repr(v: &Value) -> NormalisedTypeRepr {
    match case_name(v) {
        "Simple" => {
            // `Simple(repr: SynTypeDefnSimpleRepr, range)` — field 0 the repr.
            // Shared with the sig-side `SynTypeDefnSigRepr.Simple`, which wraps
            // the same `SynTypeDefnSimpleRepr`, via [`fcs_simple_repr`].
            fcs_simple_repr(&fields(v)[0])
        }
        "ObjectModel" => {
            // `ObjectModel(kind: SynTypeDefnKind, members: SynMemberDefn list,
            // range)` (phase 9.7). Field 0 the kind, field 1 the member list.
            let of = fields(v);
            // A delegate (`type T = delegate of int -> int`) is lowered to
            // `ObjectModel(Delegate(ty, arity), [AbstractSlot "Invoke"], _)`.
            // Keep only the signature `ty` (field 0 of the kind); the `arity`
            // and the synthetic `Invoke` slot are both derived from it.
            if case_name(&of[0]) == "Delegate" {
                let kf = fields(&of[0]);
                return NormalisedTypeRepr::Delegate(fcs_type(&kf[0]));
            }
            let kind = fcs_type_defn_kind(&of[0]);
            let members = of[1]
                .as_array()
                .expect("SynTypeDefnRepr.ObjectModel field 1 must be array")
                .iter()
                .map(fcs_member)
                .collect();
            NormalisedTypeRepr::ObjectModel { kind, members }
        }
        other => panic!("Phase 9.x: unsupported SynTypeDefnRepr case {other:?}"),
    }
}

/// Project one `SynTypeDefnSimpleRepr` — the record/union/enum/abbreviation/
/// bodyless right-hand side shared by the impl-side `SynTypeDefnRepr.Simple`
/// (phase 9) and the sig-side `SynTypeDefnSigRepr.Simple` (phase 10.14). The
/// abbreviation form (`TypeAbbrev`, `rhsType` field 1 → [`fcs_type`]) is what
/// the first sig-type slice exercises; the record / union / enum / bodyless
/// forms are reached only by the impl side today. Other simple reprs `panic!`
/// until their slices land, so an out-of-scope shape fails loudly.
fn fcs_simple_repr(simple: &Value) -> NormalisedTypeRepr {
    match case_name(simple) {
        "None" => {
            // `SynTypeDefnSimpleRepr.None(range)` — a bodyless type definition
            // (no `=`): `[<Measure>] type m`, `type Foo`, the `recover`-path
            // `type C(x)`. The range is elided.
            NormalisedTypeRepr::None
        }
        "TypeAbbrev" => {
            // `TypeAbbrev(detail: ParserDetail, rhsType: SynType, range)`.
            let tf = fields(simple);
            NormalisedTypeRepr::Abbrev(fcs_type(&tf[1]))
        }
        "Record" => {
            // `Record(accessibility, recordFields: SynField list, range)` —
            // field 0 is the repr-level access (`type R = private { … }`),
            // field 1 the field list.
            let rf = fields(simple);
            let recd_fields = rf[1]
                .as_array()
                .expect("SynTypeDefnSimpleRepr.Record field 1 must be array")
                .iter()
                .map(fcs_field)
                .collect();
            NormalisedTypeRepr::Record {
                access: fcs_access(&rf[0]),
                fields: recd_fields,
            }
        }
        "Union" => {
            // `Union(accessibility, unionCases: SynUnionCase list, range)` —
            // field 0 is the repr-level access (`type U = private | A`),
            // field 1 the case list.
            let uf = fields(simple);
            let cases = uf[1]
                .as_array()
                .expect("SynTypeDefnSimpleRepr.Union field 1 must be array")
                .iter()
                .map(fcs_union_case)
                .collect();
            NormalisedTypeRepr::Union {
                access: fcs_access(&uf[0]),
                cases,
            }
        }
        "Enum" => {
            // `Enum(cases: SynEnumCase list, range)` — field 0 is the case list.
            let ef = fields(simple);
            let cases = ef[0]
                .as_array()
                .expect("SynTypeDefnSimpleRepr.Enum field 0 must be array")
                .iter()
                .map(fcs_enum_case)
                .collect();
            NormalisedTypeRepr::Enum(cases)
        }
        "LibraryOnlyILAssembly" => {
            // `LibraryOnlyILAssembly(ilType: obj, range)` — FSharp.Core's inline
            // -IL type body `( # "instr" # )`. FCS boxes the parsed IL as `obj`,
            // which the dump cannot round-trip, so (like the expression-form
            // `SynExpr.LibraryOnlyILAssembly`) it is left unmodeled: the CST side
            // `panic!`s too, so neither side reaches the equality assertion.
            panic!("inline IL (SynTypeDefnSimpleRepr.LibraryOnlyILAssembly) is not modelled")
        }
        other => {
            panic!("Phase 9.x: unsupported SynTypeDefnSimpleRepr case {other:?}")
        }
    }
}

/// Project a `SynTypeDefnKind` (`SyntaxTree.fsi:1356`, phase 9.7). Only
/// `Unspecified` (a bare `type T = member …`) is reachable so far; the explicit
/// `Class`/`Struct`/`Interface` (9.12) and `Augmentation` (9.13) markers
/// `panic!` until their slices land.
fn fcs_type_defn_kind(v: &Value) -> NormalisedTypeDefnKind {
    match case_name(v) {
        "Unspecified" => NormalisedTypeDefnKind::Unspecified,
        // `Augmentation(range)` — `type T with member …` (phase 9.13a).
        "Augmentation" => NormalisedTypeDefnKind::Augmentation,
        // Explicit `class`/`struct`/`interface … end` kind markers (phase 9.12).
        "Class" => NormalisedTypeDefnKind::Class,
        "Struct" => NormalisedTypeDefnKind::Struct,
        "Interface" => NormalisedTypeDefnKind::Interface,
        other => panic!("Phase 9.7: unsupported SynTypeDefnKind case {other:?}"),
    }
}

/// Project one `SynMemberDefn`. The `Member(SynBinding, range)` form (9.7,
/// field 0 the binding, via [`fcs_binding`]) and `ImplicitCtor(accessibility,
/// attributes, ctorArgs: SynPat, selfIdentifier, xmlDoc, range, trivia)` (9.8a,
/// field 1 the attributes (10.7j), field 2 the args `SynPat` via [`fcs_pat`],
/// field 3 the `as` self-id) are
/// modelled, as are `val` fields, `inherit`/`ImplicitInherit` (9.11a),
/// `interface` implementations (9.11b), abstract slots, auto-properties, and
/// get/set members (9.14). Every in-scope `SynMemberDefn` case is now handled.
/// Project a `SynInterfaceImpl(interfaceTy, withKeyword, bindings, members,
/// range)` — an object expression's extra interface (`SynExpr.ObjExpr.extraImpls`)
/// — to a [`NormalisedMember::Interface`], the same shape the type-definition
/// interface member (`SynMemberDefn.Interface`) projects to. Field 0 is the
/// interface type; field 3 the `members` list; `withKeyword` (field 1) decides
/// the `members: Option` (the CST side's `INTERFACE_IMPL` has the same
/// `has_with()` discriminator — a `with` block → `Some(members)`, a bare
/// `interface I` → `None`). The `bindings` (field 2, the deprecated value form)
/// and range are elided.
fn fcs_interface_impl(v: &Value) -> NormalisedMember {
    let f = fields(v);
    let interface_type = fcs_type(&f[0]);
    let members = if f[1].is_null() {
        None
    } else {
        Some(
            f[3].as_array()
                .expect("SynInterfaceImpl field 3 (members) must be a JSON array")
                .iter()
                .map(fcs_member)
                .collect(),
        )
    };
    NormalisedMember::Interface {
        interface_type,
        members,
    }
}

fn fcs_member(v: &Value) -> NormalisedMember {
    match case_name(v) {
        "Member" => {
            let f = fields(v);
            NormalisedMember::Member(fcs_binding(&f[0]))
        }
        "ImplicitCtor" => {
            let f = fields(v);
            // `attributes` is field 1 (phase 10.7j).
            let attributes = fcs_attribute_lists(&f[1]);
            let args = fcs_pat(&f[2]);
            // `selfIdentifier: Ident option` — `null` (None) or
            // `{Case:"Some", Fields:[ident]}`.
            let self_id = if f[3].is_null() {
                None
            } else {
                match case_name(&f[3]) {
                    "Some" => Some(
                        fields(&f[3])[0]
                            .get("idText")
                            .and_then(Value::as_str)
                            .expect("ImplicitCtor selfIdentifier Some(Ident) has idText")
                            .to_string(),
                    ),
                    "None" => None,
                    other => panic!("unexpected ImplicitCtor selfIdentifier Option case {other:?}"),
                }
            };
            NormalisedMember::ImplicitCtor {
                args,
                self_id,
                attributes,
                // `SynMemberDefn.ImplicitCtor.accessibility` (field 0).
                access: fcs_access(&f[0]),
            }
        }
        "LetBindings" => {
            // `LetBindings(bindings: SynBinding list, isStatic, isRecursive,
            // range, trivia)` (phase 9.8b/9.8d). Field 0 the bindings, field 2
            // `isRecursive` (`isStatic` — field 1 — elided; for a `static do`
            // the static-ness rides on the binding's `StaticDo` leading keyword,
            // as `static let` rides on `StaticLet`). A class-body `do` (phase
            // 9.8d) is a single binding of kind `Do`, projected through the same
            // `fcs_binding`.
            let f = fields(v);
            let is_rec = f[2]
                .as_bool()
                .expect("SynMemberDefn.LetBindings.isRecursive (field 2) must be a JSON bool");
            let bindings = f[0]
                .as_array()
                .expect("SynMemberDefn.LetBindings field 0 must be array")
                .iter()
                .map(fcs_binding)
                .collect();
            NormalisedMember::LetBindings { is_rec, bindings }
        }
        "ValField" => {
            // `ValField(fieldInfo: SynField, range)` (phase 9.9b) — field 0 the
            // `SynField`, reusing `fcs_field`.
            let f = fields(v);
            NormalisedMember::ValField(fcs_field(&f[0]))
        }
        "Inherit" => {
            // `Inherit(baseType: SynType option, asIdent: Ident option, range,
            // trivia)` (phase 9.11a) — the argument-less `inherit Base` form.
            // Field 0 the base type option; `asIdent` (field 1) elided.
            let f = fields(v);
            let base_type = if f[0].is_null() {
                None
            } else {
                match case_name(&f[0]) {
                    "Some" => Some(fcs_type(&fields(&f[0])[0])),
                    "None" => None,
                    other => {
                        panic!("unexpected SynMemberDefn.Inherit baseType Option case {other:?}")
                    }
                }
            };
            NormalisedMember::Inherit { base_type }
        }
        "ImplicitInherit" => {
            // `ImplicitInherit(inheritType: SynType, inheritArgs: SynExpr,
            // inheritAlias: Ident option, range, trivia)` (phase 9.11a) — the
            // `inherit Base(args)` form. Field 0 the base type, field 1 the args
            // expr; `inheritAlias` (field 2) elided.
            let f = fields(v);
            NormalisedMember::ImplicitInherit {
                base_type: fcs_type(&f[0]),
                args: fcs_expr(&f[1]),
            }
        }
        "Interface" => {
            // `Interface(interfaceType: SynType, withKeyword: range option,
            // members: SynMemberDefns option, range)` (phase 9.11b). Field 0 the
            // interface type, field 2 the `members` option (null → None; `Some`
            // → `fields[0]` is the member-list array). `withKeyword` (field 1) and
            // range elided.
            let f = fields(v);
            let interface_type = fcs_type(&f[0]);
            let members = if f[2].is_null() {
                None
            } else {
                match case_name(&f[2]) {
                    "Some" => Some(
                        fields(&f[2])[0]
                            .as_array()
                            .expect("SynMemberDefn.Interface members Some(list) must be array")
                            .iter()
                            .map(fcs_member)
                            .collect(),
                    ),
                    "None" => None,
                    other => {
                        panic!("unexpected SynMemberDefn.Interface members Option case {other:?}")
                    }
                }
            };
            NormalisedMember::Interface {
                interface_type,
                members,
            }
        }
        "GetSetMember" => {
            // `GetSetMember(getBinding: SynBinding option, setBinding: SynBinding
            // option, range, trivia)` (phase 9.14). Each accessor binding's
            // `headPat` is `LongIdent(longDotId=[this; P], extraId=Some get/set,
            // …, args)` — we destructure it manually rather than via `fcs_pat`,
            // which panics on the `extraId`. The property path is shared; take it
            // from whichever accessor is present.
            let f = fields(v);
            let get = fcs_opt(&f[0]).map(fcs_get_set_accessor);
            let set = fcs_opt(&f[1]).map(fcs_get_set_accessor);
            let name = get
                .as_ref()
                .or(set.as_ref())
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            NormalisedMember::GetSetMember {
                name,
                get: get.map(|(_, a)| a),
                set: set.map(|(_, a)| a),
            }
        }
        "AutoProperty" => {
            // `AutoProperty(attributes, isStatic, ident, typeOpt, propKind,
            // memberFlags, memberFlagsForSet, xmlDoc, accessibility, synExpr,
            // range, trivia)` (phase 9.9c). Fields: 0 `attributes` (phase 10.7h),
            // 1 `isStatic`, 2 `ident`, 3 `typeOpt: SynType option`, 4 `propKind:
            // SynMemberKind`, 9 the `synExpr` initialiser. Flags/accessibility/
            // xmlDoc elided.
            let f = fields(v);
            let is_static = f[1]
                .as_bool()
                .expect("SynMemberDefn.AutoProperty.isStatic (field 1) must be a JSON bool");
            let name = f[2]
                .get("idText")
                .and_then(Value::as_str)
                .expect("SynMemberDefn.AutoProperty.ident (field 2) has idText")
                .to_string();
            let ty = if f[3].is_null() {
                None
            } else {
                match case_name(&f[3]) {
                    "Some" => Some(fcs_type(&fields(&f[3])[0])),
                    "None" => None,
                    other => panic!("unexpected AutoProperty typeOpt Option case {other:?}"),
                }
            };
            let prop_kind = match case_name(&f[4]) {
                "Member" => NormalisedPropKind::Member,
                "PropertyGet" => NormalisedPropKind::PropertyGet,
                "PropertySet" => NormalisedPropKind::PropertySet,
                "PropertyGetSet" => NormalisedPropKind::PropertyGetSet,
                other => panic!("unexpected AutoProperty propKind (SynMemberKind) {other:?}"),
            };
            let expr = fcs_expr(&f[9]);
            // `AutoProperty.attributes` is field 0 (phase 10.7h).
            let attributes = fcs_attribute_lists(&f[0]);
            NormalisedMember::AutoProperty {
                name,
                is_static,
                ty,
                prop_kind,
                expr,
                attributes,
                // `AutoProperty.accessibility` (field 8, a `SynValSigAccess`).
                access: fcs_val_sig_access(&f[8]),
            }
        }
        "AbstractSlot" => {
            // `AbstractSlot(slotSig: SynValSig, flags: SynMemberFlags, range,
            // trivia)` (phase 9.10c). Field 0 the `SynValSig` (name + type via
            // `fcs_val_sig`); its trivia (field 11) carries the
            // `Abstract`/`AbstractMember` leading keyword. `flags`/arity elided.
            let f = fields(v);
            let slot = &f[0];
            let (name, ty) = fcs_val_sig(slot);
            let leading_keyword = fcs_leading_keyword(
                fields(slot)[11]
                    .get("LeadingKeyword")
                    .expect("SynValSig trivia (field 11) must have LeadingKeyword"),
            );
            // `SynValSig.attributes` is field 0 (phase 10.7g).
            let attributes = fcs_attribute_lists(&fields(slot)[0]);
            NormalisedMember::AbstractSlot {
                name,
                ty,
                leading_keyword,
                attributes,
                // An impl-side abstract slot is bodyless (FCS rejects
                // `abstract M : int = 1`); only sig member sigs carry a literal.
                literal: None,
                // `SynValSig.accessibility` (field 8, a `SynValSigAccess`).
                access: fcs_val_sig_access(&fields(slot)[8]),
            }
        }
        other => panic!("Phase 9.x: unsupported SynMemberDefn case {other:?}"),
    }
}

/// Unwrap an F# `'a option` encoded in the AdjacentTag JSON: `null` / `{Case:
/// "None"}` → `None`; `{Case: "Some", Fields: [inner]}` → `Some(inner)`.
fn fcs_opt(v: &Value) -> Option<&Value> {
    if v.is_null() {
        return None;
    }
    match case_name(v) {
        "Some" => Some(&fields(v)[0]),
        "None" => None,
        other => panic!("unexpected Option case {other:?}"),
    }
}

/// Decode a `SynAccess option` field (`SyntaxTree`). `null`/`None` → `None`;
/// `Some(Public/Internal/Private)` → the corresponding [`NormalisedAccess`].
fn fcs_access(v: &Value) -> Option<NormalisedAccess> {
    fcs_opt(v).map(|inner| match case_name(inner) {
        "Public" => NormalisedAccess::Public,
        "Internal" => NormalisedAccess::Internal,
        "Private" => NormalisedAccess::Private,
        other => panic!("unexpected SynAccess case {other:?}"),
    })
}

/// Decode a `SynValSigAccess` (`SyntaxTree`), used by `SynValSig.accessibility`
/// and `SynMemberDefn.AutoProperty.accessibility`. Both `Single(access)` and
/// `GetSet(access, getter, setter)` expose the *overall* access as field 0; the
/// per-getter/setter slots are not modelled.
fn fcs_val_sig_access(v: &Value) -> Option<NormalisedAccess> {
    let f = fields(v);
    match case_name(v) {
        // `Single` and `GetSet` both carry the overall access in field 0.
        "Single" | "GetSet" => fcs_access(&f[0]),
        other => panic!("unexpected SynValSigAccess case {other:?}"),
    }
}

/// Extract the head-pattern accessibility of a `SynBinding` (a `SynPat`): FCS
/// stores a binding's access on its `headPat` — field 2 of `SynPat.Named`,
/// field 4 of `SynPat.LongIdent`. Any other head-pattern shape carries no
/// access slot.
fn fcs_pat_access(pat: &Value) -> Option<NormalisedAccess> {
    let f = fields(pat);
    match case_name(pat) {
        "Named" => fcs_access(&f[2]),
        "LongIdent" => fcs_access(&f[4]),
        _ => None,
    }
}

/// Project one get/set accessor `SynBinding` (phase 9.14) to its property path
/// and a [`NormalisedAccessor`]. The accessor's `headPat` is a `SynPat.LongIdent`
/// whose `extraId` holds the `get`/`set` keyword — `fcs_pat` panics on that, so
/// destructure the headPat directly: field 0 the `longDotId` (the shared property
/// path), field 3 the `SynArgPats.Pats` (the accessor args, whose elements have
/// no `extraId`, so `fcs_pat` is safe on them). The binding's rhs (field 9) is
/// the body. `extraId`/`SynValData`/flags/trivia are elided.
fn fcs_get_set_accessor(binding: &Value) -> (Vec<String>, NormalisedAccessor) {
    let bf = fields(binding);
    let head_pat = &bf[7];
    let hp = fields(head_pat);
    let name = fcs_syn_long_ident_segments(&hp[0]);
    let arg_case = case_name(&hp[3]);
    if arg_case != "Pats" {
        panic!("Phase 9.14: unsupported get/set accessor SynArgPats case {arg_case:?}");
    }
    let args = fields(&hp[3])[0]
        .as_array()
        .expect("SynArgPats.Pats field 0 must be a SynPat list")
        .iter()
        .map(fcs_pat)
        .collect();
    let body = fcs_expr(&bf[9]);
    // Field 4 is `SynBinding.attributes` (phase 10.7f). FCS duplicates the
    // property's leading attribute onto both accessor bindings, so each carries
    // it independently.
    let attributes = fcs_attribute_lists(&bf[4]);
    (
        name,
        NormalisedAccessor {
            attributes,
            // The accessor binding's access on its `headPat.LongIdent` (field 4).
            access: fcs_pat_access(head_pat),
            args,
            body,
        },
    )
}

/// Project a `SynValSig(attributes, ident: SynIdent, explicitTypeParams,
/// synType, arity, isInline, isMutable, xmlDoc, accessibility, synExpr, range,
/// trivia)` to its `(name, type)`. Shared by the phase-9.10c abstract slot and
/// (later) phase-10.12 `val` signatures. Field 1 is the `SynIdent` (its field 0
/// the `Ident`); field 3 the `synType`. Arity / flags / explicit typars /
/// inline / mutable / accessibility / attributes are elided.
fn fcs_val_sig(v: &Value) -> (String, NormalisedType) {
    let f = fields(v);
    let name = fcs_syn_ident_name(&f[1]);
    let ty = fcs_type(&f[3]);
    (name, ty)
}

/// The source name of a `SynIdent(ident: Ident, trivia: IdentTrivia option)` —
/// the `OriginalNotation` spelling when FCS mangled an operator (`(+)` →
/// `idText "op_Addition"` + `OriginalNotation "+"`), otherwise the
/// backtick-stripped `idText`. The single-`SynIdent` analogue of
/// [`fcs_syn_long_ident_segments`]' per-segment rule (`SynValSig.ident`, …).
fn fcs_syn_ident_name(syn_ident: &Value) -> String {
    let id_fields = fields(syn_ident);
    if let Some(trivia) = id_fields.get(1)
        && let Some(text) = ident_original_notation(trivia)
    {
        return text;
    }
    let raw = id_fields[0]
        .get("idText")
        .and_then(Value::as_str)
        .expect("SynIdent ident has idText");
    raw.strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .unwrap_or(raw)
        .to_string()
}

/// Project one `SynField(attributes, isStatic, idOpt, fieldType, isMutable,
/// xmlDoc, accessibility, range, trivia)`. Field 1 is `isStatic` (significant
/// only for a `val` field, 9.9b — always `false` for record/union fields),
/// field 2 `idOpt` (`Ident option`), field 3 the `fieldType`, field 4
/// `isMutable`.
fn fcs_field(v: &Value) -> NormalisedField {
    let f = fields(v);
    let is_static = f[1]
        .as_bool()
        .expect("SynField.isStatic (field 1) must be a JSON bool");
    // `idOpt: Ident option` — `null` (None) or `{Case:"Some", Fields:[ident]}`.
    let name = if f[2].is_null() {
        None
    } else {
        match case_name(&f[2]) {
            "Some" => Some(
                fields(&f[2])[0]
                    .get("idText")
                    .and_then(Value::as_str)
                    .expect("SynField idOpt Ident record has idText")
                    .to_string(),
            ),
            "None" => None,
            other => panic!("unexpected SynField idOpt Option case {other:?}"),
        }
    };
    let ty = fcs_type(&f[3]);
    let is_mutable = f[4]
        .as_bool()
        .expect("SynField.isMutable (field 4) must be a JSON bool");
    // `attributes` (field 0, phase 10.7) — populated for record fields (10.7b) and
    // `val` fields (10.7i); union-case `of` fields are unattributed in scope, so
    // this matches the (empty) projection our side gives them.
    NormalisedField {
        attributes: fcs_attribute_lists(&f[0]),
        // `SynField.accessibility` (field 6) — a `val` field's access; `None`
        // for record / union-case fields.
        access: fcs_access(&f[6]),
        name,
        ty,
        is_mutable,
        is_static,
    }
}

/// Project one `SynUnionCase(attributes, ident: SynIdent, caseType:
/// SynUnionCaseKind, xmlDoc, accessibility, range, trivia)` (phase 9.5). Field 1
/// is the `SynIdent` (its field 0 the `Ident`); field 2 is the
/// `SynUnionCaseKind` — `Fields(SynField list)` (field 0 the list, projected via
/// [`fcs_field`]). The FSharp.Core-only `FullType` form is a later slice.
fn fcs_union_case(v: &Value) -> NormalisedUnionCase {
    let f = fields(v);
    let ident = fields(&f[1])[0]
        .get("idText")
        .and_then(Value::as_str)
        .map(|t| {
            t.strip_prefix('`')
                .and_then(|s| s.strip_suffix('`'))
                .unwrap_or(t)
        })
        .expect("SynUnionCase ident SynIdent has idText")
        .to_string();
    let case_type = &f[2];
    let kind = match case_name(case_type) {
        "Fields" => NormalisedUnionCaseKind::Fields(
            fields(case_type)[0]
                .as_array()
                .expect("SynUnionCaseKind.Fields field 0 must be array")
                .iter()
                .map(fcs_field)
                .collect(),
        ),
        // `FullType(fullType: SynType, fullTypeInfo: SynValInfo)` — FSharp.Core's
        // `Name : topType` signature form. Field 0 is the signature `SynType`
        // (which already carries the labelled parameters); the derived
        // `fullTypeInfo` (field 1) is elided.
        "FullType" => NormalisedUnionCaseKind::FullType(fcs_type(&fields(case_type)[0])),
        other => panic!("Phase 9.5: unsupported SynUnionCaseKind case {other:?}"),
    };
    NormalisedUnionCase {
        attributes: fcs_attribute_lists(&f[0]),
        ident,
        kind,
    }
}

/// Project one `SynEnumCase(attributes, ident: SynIdent, valueExpr: SynExpr,
/// xmlDoc, range, trivia)` (phase 9.6). Field 1 is the `SynIdent` (its field 0
/// the `Ident`); field 2 is the value `SynExpr` (projected via [`fcs_expr`]).
fn fcs_enum_case(v: &Value) -> NormalisedEnumCase {
    let f = fields(v);
    let ident = fields(&f[1])[0]
        .get("idText")
        .and_then(Value::as_str)
        .map(|t| {
            t.strip_prefix('`')
                .and_then(|s| s.strip_suffix('`'))
                .unwrap_or(t)
        })
        .expect("SynEnumCase ident SynIdent has idText")
        .to_string();
    NormalisedEnumCase {
        attributes: fcs_attribute_lists(&f[0]),
        ident,
        value: fcs_expr(&f[2]),
    }
}

/// `SynBinding(access, kind, isInline, isMutable, attributes, xmlDoc, valData,
/// headPat, returnInfo, expr, range, debugPoint, trivia)`. Field offsets:
/// 2 = isInline, 3 = isMutable, 7 = headPat, 9 = expr, 12 = trivia. Phase 4.1
/// panics on any non-`Normal` binding kind (field 1) so the diff loudly flags
/// shapes the projector doesn't model yet.
fn fcs_binding(v: &Value) -> NormalisedBinding {
    let f = fields(v);
    let kind = case_name(&f[1]);
    // `Normal` is an ordinary `let`/`member`/… binding; `Do` is a class-body
    // `do <expr>` (phase 9.8d) — a `SynBinding` whose head pattern is a synthetic
    // `Const(Unit)` and whose `expr` is the `do` body, distinguished from a
    // `let () = …` only by its `Do`/`StaticDo` leading keyword. Both project
    // through the shared field offsets below. Any other kind is still unmodelled.
    if !matches!(kind, "Normal" | "Do") {
        panic!("Phase 4.1: unsupported SynBinding kind {kind:?}");
    }
    // Field 12 is `trivia: SynBindingTrivia`, a record carrying the
    // `LeadingKeyword: SynLeadingKeyword`. `IsBang`/`IsUse` are *computed
    // members* (not serialised), so we read the keyword case directly.
    let leading_keyword = fcs_leading_keyword(
        f[12]
            .get("LeadingKeyword")
            .expect("SynBindingTrivia (field 12) must have LeadingKeyword"),
    );
    let is_inline = f[2]
        .as_bool()
        .expect("SynBinding.isInline (field 2) must be a JSON bool");
    let is_mutable = f[3]
        .as_bool()
        .expect("SynBinding.isMutable (field 3) must be a JSON bool");
    let pat = fcs_pat(&f[7]);
    let expr = fcs_expr(&f[9]);
    // Field 4 is `attributes: SynAttributes` (a `SynAttributeList list`).
    let attributes = fcs_attribute_lists(&f[4]);
    // FCS stores a binding's access on its head pattern (field 7), not on
    // `SynBinding` itself (field 0 is `None` in the modern model). Read it from
    // the `SynPat.Named`/`LongIdent` head so `let private x` / `member private
    // this.M` / `private new(…)` all resolve.
    let access = fcs_pat_access(&f[7]);
    NormalisedBinding {
        leading_keyword,
        is_mutable,
        is_inline,
        attributes,
        access,
        pat,
        expr,
    }
}

/// Map a `SynLeadingKeyword` case (`SyntaxTrivia.fsi:304`) to
/// [`NormalisedLeadingKeyword`]. Only the `let`-binding-reachable variants are
/// modelled; the member/static keywords (phase 9) `panic!` so an out-of-scope
/// binding shape fails loudly.
fn fcs_leading_keyword(v: &Value) -> NormalisedLeadingKeyword {
    match case_name(v) {
        "Let" => NormalisedLeadingKeyword::Let,
        "LetRec" => NormalisedLeadingKeyword::LetRec,
        "And" => NormalisedLeadingKeyword::And,
        "Use" => NormalisedLeadingKeyword::Use,
        "UseRec" => NormalisedLeadingKeyword::UseRec,
        "LetBang" => NormalisedLeadingKeyword::LetBang,
        "UseBang" => NormalisedLeadingKeyword::UseBang,
        "AndBang" => NormalisedLeadingKeyword::AndBang,
        // `Member` — an object-model instance member (`member this.M = …`,
        // phase 9.7); `StaticMember` a `static member M = …` (phase 9.9a);
        // `Override`/`Default` (phase 9.10a) — `override`/`default this.M = …`,
        // the same `SynMemberDefn.Member` shape with a different leading keyword.
        // The remaining member keywords are later slices.
        "Member" => NormalisedLeadingKeyword::Member,
        "Static" => NormalisedLeadingKeyword::Static,
        "StaticMember" => NormalisedLeadingKeyword::StaticMember,
        // `StaticLet` / `StaticLetRec` — the head binding of a `static let` /
        // `static let rec` (phase 9.8c); `mkClassMemberLocalBindings` rewrites
        // `Let` / `LetRec` to these in place.
        "StaticLet" => NormalisedLeadingKeyword::StaticLet,
        "StaticLetRec" => NormalisedLeadingKeyword::StaticLetRec,
        // `Do` / `StaticDo` — a class-body `[static] do <expr>` binding (phase
        // 9.8d), the `do`-binding `classDefnBindings` arm.
        "Do" => NormalisedLeadingKeyword::Do,
        "StaticDo" => NormalisedLeadingKeyword::StaticDo,
        // `Synthetic` — a keyword-less binding; the object-expression
        // value-binding head (`{ new T() with X = e }`) carries it (the shared
        // `with` is not a per-binding keyword).
        "Synthetic" => NormalisedLeadingKeyword::Synthetic,
        "Override" => NormalisedLeadingKeyword::Override,
        "Default" => NormalisedLeadingKeyword::Default,
        // `New` — an explicit constructor `new(args) = …` (phase 9.10b), a
        // `SynMemberDefn.Member` whose head is the `new` keyword.
        "New" => NormalisedLeadingKeyword::New,
        // `Abstract` / `AbstractMember` — an abstract slot `abstract [member]
        // M : …` (phase 9.10c), carried in `SynValSig.trivia.LeadingKeyword`.
        "Abstract" => NormalisedLeadingKeyword::Abstract,
        "AbstractMember" => NormalisedLeadingKeyword::AbstractMember,
        "StaticAbstract" => NormalisedLeadingKeyword::StaticAbstract,
        "StaticAbstractMember" => NormalisedLeadingKeyword::StaticAbstractMember,
        // `Extern` — an `extern` DllImport prototype (FCS's `cPrototype`),
        // lowered to a `SynModuleDecl.Let` binding with this leading keyword.
        "Extern" => NormalisedLeadingKeyword::Extern,
        other => panic!("unsupported SynLeadingKeyword case {other:?} (phase 9 member keyword?)"),
    }
}

/// Project a `SynAttribute` record (a JSON object, not a DU case) to
/// [`NormalisedAttribute`]. `TypeName` is a `SynLongIdent`; `ArgExpr` a
/// `SynExpr` (bare attributes carry `mkSynUnit` ⇒ `Const(Unit)`); `Target` an
/// `Ident option` (`null` ⇒ `None`, else the `idText`).
/// `AppliesToGetterAndSetter` and `Range` are elided.
fn fcs_attribute(v: &Value) -> NormalisedAttribute {
    let type_name = fcs_syn_long_ident(
        v.get("TypeName")
            .expect("SynAttribute record has TypeName field"),
    );
    let arg = fcs_expr(
        v.get("ArgExpr")
            .expect("SynAttribute record has ArgExpr field"),
    );
    // `Target: Ident option` — `null` (None) or `{Case:"Some", Fields:[ident]}`,
    // the same `Ident option` shape as `SynField.idOpt`.
    let target = match v.get("Target") {
        Some(t) if !t.is_null() => match case_name(t) {
            "Some" => Some(
                fields(t)[0]
                    .get("idText")
                    .and_then(Value::as_str)
                    .expect("SynAttribute.Target Some(Ident) has idText")
                    .to_string(),
            ),
            "None" => None,
            other => panic!("unexpected SynAttribute.Target Option case {other:?}"),
        },
        _ => None,
    };
    NormalisedAttribute {
        type_name,
        target,
        arg,
    }
}

/// Project a `SynAttributes` value (a `SynAttributeList list`) to the
/// `Vec<Vec<NormalisedAttribute>>` carrier shape: one inner `Vec` per
/// `SynAttributeList` (a JSON record `{ "Attributes": [...], ... }`), each
/// holding that list's `SynAttribute`s. Shared by the let-binding carrier
/// (`SynBinding.attributes`) and the parameter-pattern carrier
/// (`SynPat.Attrib.attributes`).
fn fcs_attribute_lists(v: &Value) -> Vec<Vec<NormalisedAttribute>> {
    v.as_array()
        .expect("SynAttributes must be a SynAttributeList array")
        .iter()
        .map(|list| {
            list.get("Attributes")
                .and_then(Value::as_array)
                .expect("SynAttributeList.Attributes must be array")
                .iter()
                .map(fcs_attribute)
                .collect()
        })
        .collect()
}

/// Read a `SynLongIdent` DU value (`SynLongIdent of id: Ident list *
/// dotRanges * trivia: IdentTrivia option list`) into its `Ident.idText`
/// segments, preferring the trivia's `OriginalNotation` spelling when FCS
/// mangled an operator ident (same rule as the inline `SynExpr.LongIdent`
/// reader).
fn fcs_syn_long_ident(v: &Value) -> Vec<String> {
    let li = fields(v);
    let idents = li[0]
        .as_array()
        .expect("SynLongIdent ident list must be array");
    let trivia = li[2]
        .as_array()
        .expect("SynLongIdent trivia list must be array");
    idents
        .iter()
        .enumerate()
        .map(|(i, id)| {
            if let Some(tv) = trivia.get(i)
                && let Some(text) = ident_original_notation(tv)
            {
                return text;
            }
            id.get("idText")
                .and_then(Value::as_str)
                .expect("Ident record has idText")
                .to_string()
        })
        .collect()
}

fn fcs_pat(v: &Value) -> NormalisedPat {
    let case = case_name(v);
    match case {
        "Wild" => {
            // `SynPat.Wild(range)` — single-field DU case carrying only
            // the range. Our normaliser elides ranges, so the variant has
            // no payload.
            NormalisedPat::Wildcard
        }
        "Named" => {
            // `SynPat.Named(SynIdent, isThisVal, accessibility, range)`.
            // Field 0 is the SynIdent; SynIdent is itself `SynIdent(ident,
            // trivia)` where `ident` is the `Ident` record (with `idText`) and
            // `trivia` an `IdentTrivia option`. A nullary parenthesised operator
            // name (`let (+) = …`) reduces to `SynPat.Named` whose `idText` is
            // the *mangled* `op_Addition` and whose trivia is
            // `OriginalNotationWithParen "+"`; prefer that source spelling so the
            // projection round-trips the operator (our green tree stores the raw
            // operator token under `IDENT_TOK`, parens stripped), exactly as
            // [`fcs_syn_long_ident_segments`] does for the `LongIdent` form. A
            // plain ident carries no such trivia, so it falls back to `idText`.
            let syn_ident_fields = fields(&fields(v)[0]);
            let id_text = ident_original_notation(&syn_ident_fields[1]).unwrap_or_else(|| {
                syn_ident_fields[0]
                    .get("idText")
                    .and_then(Value::as_str)
                    .expect("SynPat.Named's Ident record has idText")
                    .to_string()
            });
            NormalisedPat::Named(id_text)
        }
        "LongIdent" => {
            // `SynPat.LongIdent(longDotId: SynLongIdent, extraId: Ident option,
            //                   typars: SynValTyparDecls option,
            //                   args: SynArgPats, accessibility, range)`.
            // We project the function-form binding head / applied union-case
            // pattern: the path segments, the explicit value-typar decls (field 2,
            // generic heads), and the `SynArgPats` args (the curried `Pats` list
            // or the named-field `NamePatPairs` group). `extraId` (a deprecated
            // member-binding slot) and `accessibility` are elided; `extraId`
            // panics if ever populated so the diff harness flags an unhandled
            // shape. Any other `SynArgPats` case panics too.
            let f = fields(v);
            let head: Vec<String> = fcs_syn_long_ident_segments(&f[0]);
            if !f[1].is_null() {
                panic!("Phase 4.4: unsupported SynPat.LongIdent extraId {:?}", f[1]);
            }
            // `typars: SynValTyparDecls option` — explicit value-typar
            // declarations on the head (`let f<'a> …`, `let h<'a> = …`).
            // Projected to the flat typar list via [`fcs_pat_typar_decls`]; the
            // synthetic `noInferredTypars` marker an explicit constructor head
            // (phase 9.10b) carries — `Some(SynValTyparDecls(None, _))` — holds
            // no real decls, so it flattens to empty, matching our side (no
            // `TYPAR_DECLS` node).
            let typars = fcs_pat_typar_decls(&f[2]);
            // `accessibility: SynAccess option`. An access-modified explicit
            // constructor head (phase 9.10b — `private new(…)`) carries
            // `Some(Private/Internal/Public)`; accessibility is elided throughout
            // the normaliser, so elide it here too (our side consumes the modifier
            // as an `ACCESS_TOK` and likewise elides it).
            let arg_pats = &f[3];
            let args = match case_name(arg_pats) {
                "Pats" => {
                    // `SynArgPats.Pats(pats: SynPat list)` — field 0 is the
                    // curried arg list.
                    let pats_list = fields(arg_pats)[0]
                        .as_array()
                        .expect("SynArgPats.Pats field 0 must be a SynPat list");
                    NormalisedArgPats::Pats(pats_list.iter().map(fcs_pat).collect())
                }
                "NamePatPairs" => {
                    // `SynArgPats.NamePatPairs(pats: NamePatPairField list, range,
                    // trivia)` — the named-field union-case form `Case (field =
                    // pat; …)`. Field 0 is the field list; each
                    // `NamePatPairField(fieldName, eqRange, range, pat, sep)`
                    // (`SyntaxTree.fsi:1068`) has its single-segment `SynLongIdent`
                    // name at field 0 and value `SynPat` at field 3. The range
                    // and trivia (`ParenRange`) are elided.
                    let field_list = fields(arg_pats)[0]
                        .as_array()
                        .expect("SynArgPats.NamePatPairs field 0 must be a NamePatPairField list");
                    NormalisedArgPats::NamePatPairs(
                        field_list
                            .iter()
                            .map(|npf| {
                                let npf_fields = fields(npf);
                                let segs = fcs_syn_long_ident_segments(&npf_fields[0]);
                                debug_assert_eq!(
                                    segs.len(),
                                    1,
                                    "namePatPair field name is a single ident, got {segs:?}",
                                );
                                let name = segs.into_iter().next().unwrap_or_default();
                                let pat = fcs_pat(&npf_fields[3]);
                                (name, pat)
                            })
                            .collect(),
                    )
                }
                other => panic!("Phase 4.4: unsupported SynArgPats case {other:?}"),
            };
            NormalisedPat::LongIdent { head, typars, args }
        }
        "Paren" => {
            // `SynPat.Paren(pat: SynPat, range: range)`
            // (`SyntaxTree.fsi`, `SynPat.Paren`). Field 0 is the inner pat.
            // FCS preserves the `Paren` wrapper in the AST rather than
            // folding it away, so we mirror that.
            let f = fields(v);
            NormalisedPat::Paren(Box::new(fcs_pat(&f[0])))
        }
        "Const" => {
            // `SynPat.Const(constant: SynConst, range: range)`. Field 0 is
            // the `SynConst` — exactly the same payload shape as
            // `SynExpr.Const`, so we reuse `fcs_const`.
            let f = fields(v);
            NormalisedPat::Const(fcs_const(&f[0]))
        }
        "Null" => {
            // `SynPat.Null(range: range)` — single-field DU case carrying
            // only the range. No payload after range elision.
            NormalisedPat::Null
        }
        "Typed" => {
            // `SynPat.Typed(pat: SynPat, targetType: SynType, range: range)`
            // (`SyntaxTree.fsi:1113`). Field 0 is the annotated pattern;
            // field 1 is the `SynType`. Reuses the type-side `fcs_type`
            // projector.
            let f = fields(v);
            NormalisedPat::Typed {
                pat: Box::new(fcs_pat(&f[0])),
                ty: fcs_type(&f[1]),
            }
        }
        "Tuple" => {
            // `SynPat.Tuple of isStruct: bool * elementPats: SynPat list *
            //                  commaRanges: range list * range: range`.
            // Field 0 is `isStruct` (`true` for `struct (x, y)`, `false` for a
            // plain `x, y` / `(x, y)`), field 1 the element list. Comma ranges
            // and outer range are elided.
            let f = fields(v);
            let is_struct = f[0]
                .as_bool()
                .expect("SynPat.Tuple field 0 is isStruct bool");
            let pats_list = f[1]
                .as_array()
                .expect("SynPat.Tuple field 1 is SynPat list");
            let elements = pats_list.iter().map(fcs_pat).collect();
            NormalisedPat::Tuple {
                is_struct,
                elements,
            }
        }
        "As" => {
            // `SynPat.As(lhsPat: SynPat, rhsPat: SynPat, range: range)`
            // (`SyntaxTree.fsi:1128`). Field 0 is the left operand, field 1
            // the right operand (a `constrPattern`-level pattern). Range
            // elided.
            let f = fields(v);
            NormalisedPat::As {
                lhs: Box::new(fcs_pat(&f[0])),
                rhs: Box::new(fcs_pat(&f[1])),
            }
        }
        "ArrayOrList" => {
            // `SynPat.ArrayOrList of isArray: bool * elementPats: SynPat list
            //                       * range: range` (`SyntaxTree.fsi:1146`).
            // Field 0 is `isArray`, field 1 the element list. Range elided.
            let f = fields(v);
            let is_array = f[0]
                .as_bool()
                .expect("SynPat.ArrayOrList field 0 is isArray bool");
            let pats_list = f[1]
                .as_array()
                .expect("SynPat.ArrayOrList field 1 is SynPat list");
            let elements = pats_list.iter().map(fcs_pat).collect();
            NormalisedPat::ArrayOrList { is_array, elements }
        }
        "Record" => {
            // `SynPat.Record of fieldPats: NamePatPairField list * range`
            // (`SyntaxTree.fsi:1149`). Field 0 is the field list; each
            // `NamePatPairField(longId, eqRange, fieldRange, pat, sepRange)`
            // (`SyntaxTree.fsi:1068`) has its `SynLongIdent` name at field 0
            // and value `SynPat` at field 3. Ranges/trivia elided.
            let f = fields(v);
            let field_list = f[0]
                .as_array()
                .expect("SynPat.Record field 0 is NamePatPairField list");
            let record_fields = field_list
                .iter()
                .map(|npf| {
                    let npf_fields = fields(npf);
                    let name = fcs_syn_long_ident_segments(&npf_fields[0]);
                    let pat = fcs_pat(&npf_fields[3]);
                    (name, pat)
                })
                .collect();
            NormalisedPat::Record {
                fields: record_fields,
            }
        }
        "IsInst" => {
            // `SynPat.IsInst(pat: SynType, range: range)`
            // (`SyntaxTree.fsi:1158`). Field 0 is the tested `SynType` (despite
            // the field's `pat` name), projected via the type-side `fcs_type`.
            let f = fields(v);
            NormalisedPat::IsInst {
                ty: fcs_type(&f[0]),
            }
        }
        "ListCons" => {
            // `SynPat.ListCons(lhsPat, rhsPat, range, trivia)`
            // (`SyntaxTree.fsi`). Field 0 is the head, field 1 the tail; the
            // range and `ColonColonRange` trivia are elided.
            let f = fields(v);
            NormalisedPat::ListCons {
                lhs: Box::new(fcs_pat(&f[0])),
                rhs: Box::new(fcs_pat(&f[1])),
            }
        }
        "Ands" => {
            // `SynPat.Ands(pats: SynPat list, range: range)`
            // (`SyntaxTree.fsi`). Field 0 is the flat operand list; the range
            // is elided.
            let f = fields(v);
            let pats_list = f[0]
                .as_array()
                .expect("SynPat.Ands field 0 must be a SynPat list");
            NormalisedPat::Ands {
                pats: pats_list.iter().map(fcs_pat).collect(),
            }
        }
        "Or" => {
            // `SynPat.Or(lhsPat, rhsPat, range, trivia)` (`SyntaxTree.fsi:1119`).
            // Field 0 is the left operand, field 1 the right; the range and
            // `SynPatOrTrivia` (`BarRange`) are elided.
            let f = fields(v);
            NormalisedPat::Or {
                lhs: Box::new(fcs_pat(&f[0])),
                rhs: Box::new(fcs_pat(&f[1])),
            }
        }
        "Attrib" => {
            // `SynPat.Attrib(pat: SynPat, attributes: SynAttributes, range)`
            // (`SyntaxTree.fsi:1116`). Field 0 is the inner pattern; field 1 is
            // the `SynAttributeList list`, projected via `fcs_attribute_lists`
            // (same shape as the let-binding carrier). The range is elided.
            let f = fields(v);
            NormalisedPat::Attrib {
                pat: Box::new(fcs_pat(&f[0])),
                attributes: fcs_attribute_lists(&f[1]),
            }
        }
        "OptionalVal" => {
            // `SynPat.OptionalVal(ident: Ident, range)` (`SyntaxTree.fsi:1155`)
            // — the optional-argument pattern `?ident`. Field 0 is the bare
            // `Ident` record (the `?` is not part of it); FCS strips backticks
            // in `idText`, matching our `IDENT_TOK`-text projection. The range
            // is elided.
            let f = fields(v);
            let id_text = f[0]
                .get("idText")
                .and_then(Value::as_str)
                .expect("SynPat.OptionalVal field 0 is an Ident record with idText")
                .to_string();
            NormalisedPat::OptionalVal(id_text)
        }
        "QuoteExpr" => {
            // `SynPat.QuoteExpr(expr: SynExpr, range)` (`SyntaxTree.fsi:1161`) —
            // a `<@ … @>` quotation in pattern position (a parameterised
            // active-pattern argument). Field 0 is the `SynExpr` (a
            // `SynExpr.Quote`); reuse the expr projector, mirroring the
            // `Pat::Quote` arm in `from_cst`.
            let f = fields(v);
            NormalisedPat::QuoteExpr(Box::new(fcs_expr(&f[0])))
        }
        other => panic!("Phase 4.1: unsupported SynPat case {other:?}"),
    }
}

fn fcs_expr(v: &Value) -> NormalisedExpr {
    let case = case_name(v);
    match case {
        "Const" => {
            // `SynExpr.Const of constant * range`. Field 0 is the SynConst.
            let fields = fields(v);
            NormalisedExpr::Const(fcs_const(&fields[0]))
        }
        // `SynExpr.Null of range` — the `null` literal expression. No
        // payload beyond the elided range.
        "Null" => NormalisedExpr::Null,
        "Ident" => {
            // `SynExpr.Ident of ident: Ident` (`SyntaxTree.fsi:805`).
            // `Ident` serialises as a record with `idText` and `idRange`;
            // FCS strips backticks before storing, so `idText` is what our
            // backtick-stripping normaliser must match.
            let fields = fields(v);
            let id_text = fields[0]
                .get("idText")
                .and_then(Value::as_str)
                .expect("SynExpr.Ident field is Ident record with idText")
                .to_string();
            NormalisedExpr::Ident(id_text)
        }
        "Typar" => {
            // `SynExpr.Typar of typar: SynTypar * range` — the F# 7 typar
            // expression `'T` (`pars.fsy:5263 QUOTE ident`). Field 0 is the
            // `SynTypar`; we carry only its name (the static-req is always
            // `None` and is elided along with the range).
            let fields = fields(v);
            NormalisedExpr::Typar(fcs_syntypar(&fields[0]).name)
        }
        "Paren" => {
            // `SynExpr.Paren of expr * leftParenRange *
            //                  rightParenRange option * range`
            // (`SyntaxTree.fsi:598`). Field 0 is the wrapped expression;
            // the paren-range and outer-range fields are range data we
            // elide.
            let fields = fields(v);
            NormalisedExpr::Paren(Box::new(fcs_expr(&fields[0])))
        }
        "TraitCall" => {
            // `SynExpr.TraitCall of supportTys: SynType * traitSig:
            //                       SynMemberSig * argExpr: SynExpr * range`
            // — an SRTP trait call. Field 0 is the support type — a `SynType.Var`
            // for a single head typar `^a`, or `SynType.Paren(SynType.Or(…))` for
            // the alternatives `((^a or int) : …)`, flattened to the operand list by
            // the shared `fcs_support_types`; field 1 the member signature (the
            // same `classMemberSpfn` payload as the SRTP member constraint),
            // field 2 the argument expression. The range is elided.
            let f = fields(v);
            NormalisedExpr::TraitCall {
                support: fcs_support_types(&f[0]),
                member: Box::new(fcs_member_sig(&f[1])),
                arg: Box::new(fcs_expr(&f[2])),
            }
        }
        "Tuple" => {
            // `SynExpr.Tuple of isStruct: bool * exprs: SynExpr list *
            //                  commaRanges: range list * range: range`.
            // Field 0 is `isStruct`, field 1 is the element list. Comma
            // ranges and outer range are elided.
            let fields = fields(v);
            let is_struct = fields[0]
                .as_bool()
                .expect("SynExpr.Tuple field 0 is isStruct bool");
            let exprs = fields[1]
                .as_array()
                .expect("SynExpr.Tuple field 1 is expr list");
            let elements = exprs.iter().map(fcs_expr).collect();
            NormalisedExpr::Tuple {
                is_struct,
                elements,
            }
        }
        "LongIdent" => {
            // `SynExpr.LongIdent of isOptional: bool * longDotId: SynLongIdent *
            //                       altNameRefCell * range`.
            // Field 1 is the SynLongIdent, itself a DU case `SynLongIdent of
            // id: Ident list * dotRanges: range list *
            // trivia: IdentTrivia option list` — field 0 is the idents and
            // field 2 is the parallel trivia list (one slot per ident).
            //
            // The trivia stashes the *original* source spelling of an ident
            // when FCS rewrote it: `mkSynOperator` for `a + b` mangles
            // `Ident.idText` to `op_Addition` and stamps
            // `Some (IdentTrivia.OriginalNotation "+")` into the trivia slot,
            // so the formatter can round-trip the source. Our Rust-side
            // green tree carries the original text directly in the
            // `IDENT_TOK`; for the diff to line up we must unwrap the
            // trivia and prefer it over `idText` when present.
            let fields = fields(v);
            let segments = fcs_syn_long_ident_segments(&fields[1]);
            NormalisedExpr::LongIdent(segments)
        }
        "App" => {
            // `SynExpr.App of flag: ExprAtomicFlag * isInfix: bool *
            //                 funcExpr: SynExpr * argExpr: SynExpr * range: range`.
            // `ExprAtomicFlag` is a .NET enum (`Atomic = 0 | NonAtomic = 1`);
            // System.Text.Json emits it as the underlying int. `is_atomic`
            // is the `flag == 0` projection. Fields 2 / 3 are the func/arg
            // sub-expressions; field 4 (range) is elided.
            let f = fields(v);
            let flag = f[0]
                .as_i64()
                .expect("SynExpr.App field 0 (ExprAtomicFlag) must be a JSON integer");
            let is_atomic = flag == 0;
            let is_infix = f[1]
                .as_bool()
                .expect("SynExpr.App field 1 (isInfix) must be a JSON bool");
            NormalisedExpr::App {
                is_atomic,
                is_infix,
                func: Box::new(fcs_expr(&f[2])),
                arg: Box::new(fcs_expr(&f[3])),
            }
        }
        "DotGet" => {
            // `SynExpr.DotGet of expr: SynExpr * rangeOfDot: range *
            //                    longDotId: SynLongIdent * range`
            // (`SyntaxTree.fsi:822`). Field 0 is the LHS expr, field 2 is the
            // member-path `SynLongIdent`; the dot-range and outer range are
            // elided. FCS keeps an identifier chain `a.b.c` as `LongIdent`
            // (not `DotGet`), so this arm only sees non-ident LHSs.
            let f = fields(v);
            NormalisedExpr::DotGet {
                expr: Box::new(fcs_expr(&f[0])),
                long_dot_id: fcs_syn_long_ident_segments(&f[2]),
            }
        }
        "Dynamic" => {
            // `SynExpr.Dynamic of funcExpr: SynExpr * qmarkRange: range *
            //                     argExpr: SynExpr * range` — the `a?b` dynamic
            // lookup. Field 0 is the LHS, field 2 the argument (an `Ident` member
            // name or a `Paren`); the qmark range and outer range are elided.
            let f = fields(v);
            NormalisedExpr::Dynamic {
                lhs: Box::new(fcs_expr(&f[0])),
                arg: Box::new(fcs_expr(&f[2])),
            }
        }
        "DotLambda" => {
            // `SynExpr.DotLambda of expr: SynExpr * range: range *
            //                       trivia: SynExprDotLambdaTrivia`
            // (`SyntaxTree.fsi:826`). Field 0 is the body (the `atomicExpr`
            // after `_.`); the range and trivia are elided. The synthesised
            // lambda parameter is introduced post-parse, so it is absent here.
            let f = fields(v);
            NormalisedExpr::DotLambda {
                expr: Box::new(fcs_expr(&f[0])),
            }
        }
        "DotIndexedGet" => {
            // `SynExpr.DotIndexedGet of objectExpr: SynExpr *
            //                           indexArgs: SynExpr * dotRange: range *
            //                           range` (`SyntaxTree.fsi:834`). Field 0
            // is the indexed object, field 1 the index args (a `Tuple` for
            // `arr.[i, j]`); the two ranges are elided.
            let f = fields(v);
            NormalisedExpr::DotIndexedGet {
                object: Box::new(fcs_expr(&f[0])),
                index: Box::new(fcs_expr(&f[1])),
            }
        }
        "IndexRange" => {
            // `SynExpr.IndexRange of expr1: SynExpr option * opm: range *
            //  expr2: SynExpr option * range1 * range2 * range`
            // (`SyntaxTree.fsi:690`). Field 0 is the lower bound option,
            // field 2 the upper bound option; the operator/aux ranges are
            // elided. System.Text.Json serialises an `option` as `null`
            // (None) or `{Case:"Some", Fields:[expr]}` (Some).
            let f = fields(v);
            let bound = |opt: &Value| (!opt.is_null()).then(|| Box::new(fcs_expr(&fields(opt)[0])));
            NormalisedExpr::IndexRange {
                lower: bound(&f[0]),
                upper: bound(&f[2]),
            }
        }
        "IndexFromEnd" => {
            // `SynExpr.IndexFromEnd of expr: SynExpr * range` (`SyntaxTree.fsi`).
            // Field 0 is the from-end bound expression; the range is elided.
            NormalisedExpr::IndexFromEnd {
                expr: Box::new(fcs_expr(&fields(v)[0])),
            }
        }
        "AddressOf" => {
            // `SynExpr.AddressOf of isByref: bool * expr: SynExpr *
            //                       opRange: range * range: range`
            // (`SyntaxTree.fsi:735`). Field 0 is `isByref` (`&` =>
            // true, `&&` => false), field 1 is the wrapped expr;
            // the two ranges are elided.
            let f = fields(v);
            let is_byref = f[0]
                .as_bool()
                .expect("SynExpr.AddressOf field 0 (isByref) must be a JSON bool");
            NormalisedExpr::AddressOf {
                is_byref,
                expr: Box::new(fcs_expr(&f[1])),
            }
        }
        "New" => {
            // `SynExpr.New of isProtected: bool * targetType: SynType *
            //                 expr: SynExpr * range: range`
            // (`SyntaxTree.fsi:642`). Field 0 is `isProtected` (always
            // `false` for the expression-level `new T(args)` production),
            // field 1 the constructed type, field 2 the constructor args
            // (`Const Unit` for `()`, `Paren(Tuple)` for `(a, b)`); range
            // is elided.
            let f = fields(v);
            let is_protected = f[0]
                .as_bool()
                .expect("SynExpr.New field 0 (isProtected) must be a JSON bool");
            NormalisedExpr::New {
                is_protected,
                ty: fcs_type(&f[1]),
                arg: Box::new(fcs_expr(&f[2])),
            }
        }
        "ObjExpr" => {
            // `SynExpr.ObjExpr of objType: SynType * argOptions: (SynExpr *
            //   Ident option) option * withKeyword * bindings: SynBinding list *
            //   members: SynMemberDefns * extraImpls: SynInterfaceImpl list *
            //   newExprRange * range` (`SyntaxTree.fsi:645`).
            let f = fields(v);
            let ty = fcs_type(&f[0]);
            // `argOptions` (field 1): `null` (the bare `new T with …`, no parens)
            // or `{Case:"Some", Fields:[[<SynExpr>, <Ident option>]]}` — a tuple
            // of the constructor-args expression and an optional base name. We
            // project only the expression (the `as base` name is elided).
            let arg = if f[1].is_null() {
                None
            } else {
                let tuple = fields(&f[1])[0].as_array().expect(
                    "SynExpr.ObjExpr argOptions Some payload must be an (expr, ident) tuple",
                );
                Some(Box::new(fcs_expr(&tuple[0])))
            };
            // The value-binding form (field 3 `bindings`, `SynBinding list`):
            // `{ new T() with X = e [and …] }` (FCS's `objExprBindings: OWITH
            // localBindings OEND`). Each head binding carries
            // `SynLeadingKeyword.Synthetic`; `and`-chained ones carry `And`. The
            // shared `fcs_binding` reads the leading keyword from the binding's
            // own trivia, so no head/tail bookkeeping is needed here.
            let bindings = f[3]
                .as_array()
                .expect("SynExpr.ObjExpr field 3 (bindings) must be a JSON array")
                .iter()
                .map(fcs_binding)
                .collect();
            let members = f[4]
                .as_array()
                .expect("SynExpr.ObjExpr field 4 (members) must be a JSON array")
                .iter()
                .map(fcs_member)
                .collect();
            // Extra interface implementations (field 5 `extraImpls`,
            // `SynInterfaceImpl list`) — each projected to a
            // `NormalisedMember::Interface`, matching the CST side's
            // `INTERFACE_IMPL` children.
            let extra_impls = f[5]
                .as_array()
                .expect("SynExpr.ObjExpr field 5 (extraImpls) must be a JSON array")
                .iter()
                .map(fcs_interface_impl)
                .collect();
            NormalisedExpr::ObjExpr {
                ty,
                arg,
                bindings,
                members,
                extra_impls,
            }
        }
        "InferredUpcast" => {
            // `SynExpr.InferredUpcast of expr: SynExpr * range`
            // (`SyntaxTree.fsi:867`). Field 0 is the coerced expr; the
            // range is elided. No target type — the `upcast` form leaves it
            // to inference (contrast `SynExpr.Upcast`, the `:>` infix form).
            let f = fields(v);
            NormalisedExpr::InferredUpcast {
                expr: Box::new(fcs_expr(&f[0])),
            }
        }
        "InferredDowncast" => {
            // `SynExpr.InferredDowncast of expr: SynExpr * range`
            // (`SyntaxTree.fsi:870`). Field 0 is the coerced expr; the range
            // is elided. The typeless sibling of the `:?>` infix downcast.
            let f = fields(v);
            NormalisedExpr::InferredDowncast {
                expr: Box::new(fcs_expr(&f[0])),
            }
        }
        "Lazy" => {
            // `SynExpr.Lazy of expr: SynExpr * range` (`SyntaxTree.fsi:873`) —
            // the `lazy e` delayed-computation prefix. Field 0 is the delayed
            // expr; the range is elided.
            let f = fields(v);
            NormalisedExpr::Lazy {
                expr: Box::new(fcs_expr(&f[0])),
            }
        }
        "Assert" => {
            // `SynExpr.Assert of expr: SynExpr * range` (`SyntaxTree.fsi:876`) —
            // the `assert e` runtime-assertion prefix. Field 0 is the asserted
            // expr; the range is elided.
            let f = fields(v);
            NormalisedExpr::Assert {
                expr: Box::new(fcs_expr(&f[0])),
            }
        }
        "Fixed" => {
            // `SynExpr.Fixed of expr: SynExpr * range` (`SyntaxTree.fsi:966`) —
            // the `fixed e` pinning prefix. Field 0 is the pinned expr (a full
            // `declExpr`); the range is elided.
            let f = fields(v);
            NormalisedExpr::Fixed {
                expr: Box::new(fcs_expr(&f[0])),
            }
        }
        "TypeApp" => {
            // `SynExpr.TypeApp of expr: SynExpr * lessRange: range *
            //                     typeArgs: SynType list * commaRanges: range list *
            //                     greaterRange: range option * typeArgsRange: range *
            //                     range` (`SyntaxTree.fsi:749`). Field 0 is the
            // type-applied head expr, field 2 the `SynType list` of type
            // arguments; the two ranges, the comma list, and the
            // `greaterRange` option are elided.
            let f = fields(v);
            let type_args = f[2]
                .as_array()
                .expect("SynExpr.TypeApp field 2 (typeArgs) must be a JSON array")
                .iter()
                .map(fcs_type)
                .collect();
            NormalisedExpr::TypeApp {
                expr: Box::new(fcs_expr(&f[0])),
                type_args,
            }
        }
        "Typed" => {
            // `SynExpr.Typed of expr: SynExpr * targetType: SynType *
            //                   range: range` (`SyntaxTree.fsi:626`).
            // Field 0 is the wrapped expr, field 1 is the type; range
            // is elided.
            let f = fields(v);
            NormalisedExpr::Typed {
                expr: Box::new(fcs_expr(&f[0])),
                ty: fcs_type(&f[1]),
            }
        }
        "TypeTest" => {
            // `SynExpr.TypeTest of expr: SynExpr * targetType: SynType *
            //                      range: range` (`SyntaxTree.fsi:858`) — the
            // `e :? T` operator. Same field layout as `Typed`; range elided.
            let f = fields(v);
            NormalisedExpr::TypeTest {
                expr: Box::new(fcs_expr(&f[0])),
                ty: fcs_type(&f[1]),
            }
        }
        "Upcast" => {
            // `SynExpr.Upcast of expr: SynExpr * targetType: SynType *
            //                    range: range` (`SyntaxTree.fsi:861`) — the
            // `e :> T` operator. Same field layout as `Typed`; range elided.
            let f = fields(v);
            NormalisedExpr::Upcast {
                expr: Box::new(fcs_expr(&f[0])),
                ty: fcs_type(&f[1]),
            }
        }
        "Downcast" => {
            // `SynExpr.Downcast of expr: SynExpr * targetType: SynType *
            //                      range: range` (`SyntaxTree.fsi:864`) — the
            // `e :?> T` operator. Same field layout as `Typed`; range elided.
            let f = fields(v);
            NormalisedExpr::Downcast {
                expr: Box::new(fcs_expr(&f[0])),
                ty: fcs_type(&f[1]),
            }
        }
        "IfThenElse" => {
            // `SynExpr.IfThenElse of ifExpr: SynExpr * thenExpr: SynExpr *
            //                         elseExpr: SynExpr option *
            //                         spIfToThen: DebugPointAtBinding *
            //                         isFromErrorRecovery: bool *
            //                         range: range *
            //                         trivia: SynExprIfThenElseTrivia`
            // (`SyntaxTree.fsi:790`). Fields 0/1 are condition/then-branch;
            // field 2 is the optional else-branch. The dumper's
            // `FSharp.SystemTextJson` config emits `Some _` in AdjacentTag
            // form (`{Case: "Some", Fields: [<expr>]}`) but `None` as
            // plain JSON `null`, so accept both. The remaining fields
            // (debug-point, recovery flag, range, trivia) are elided.
            let f = fields(v);
            let else_branch = if f[2].is_null() {
                None
            } else {
                match case_name(&f[2]) {
                    "Some" => Some(Box::new(fcs_expr(&fields(&f[2])[0]))),
                    "None" => None,
                    other => panic!("unexpected SynExpr.IfThenElse elseExpr Option case {other:?}"),
                }
            };
            NormalisedExpr::IfThenElse {
                condition: Box::new(fcs_expr(&f[0])),
                then_branch: Box::new(fcs_expr(&f[1])),
                else_branch,
            }
        }
        "Sequential" => {
            // `SynExpr.Sequential of debugPoint * isTrueSeq * expr1 *
            //                         expr2 * range * trivia`
            // (`SyntaxTree.fsi:704`). Fields 2/3 are the two children;
            // when expr2 is itself a Sequential the structure is
            // right-leaning. Flatten to match the n-ary green-tree
            // shape produced by `parse_if_then_else`.
            let f = fields(v);
            let mut acc = vec![fcs_expr(&f[2])];
            let mut tail = &f[3];
            while case_name(tail) == "Sequential" {
                let tf = fields(tail);
                acc.push(fcs_expr(&tf[2]));
                tail = &tf[3];
            }
            acc.push(fcs_expr(tail));
            NormalisedExpr::Sequential(acc)
        }
        "InterpolatedString" => {
            // `SynExpr.InterpolatedString of contents *
            //                                synStringKind: SynStringKind *
            //                                range: range`
            // (`SyntaxTree.fsi:970`). Field 0 is the parts list, field 1
            // is the SynStringKind discriminant, field 2 is the range
            // (elided).
            let f = fields(v);
            let raw_parts = f[0]
                .as_array()
                .expect("SynExpr.InterpolatedString field 0 (parts) must be an array");
            let parts = raw_parts.iter().map(fcs_interp_part).collect();
            let kind = fcs_syn_string_kind(&f[1]);
            NormalisedExpr::InterpolatedString { parts, kind }
        }
        "Lambda" => {
            // `SynExpr.Lambda of fromMethod: bool * inLambdaSeq: bool *
            //                    args: SynSimplePats * body: SynExpr *
            //                    parsedData: (SynPat list * SynExpr) option *
            //                    range: range * trivia`
            // (`SyntaxTree.fsi:825`). The runtime `args`/`body` fields
            // encode the curried form (one `SynSimplePats.SimplePats`
            // arg per arrow level, with inner `Lambda`s for the rest);
            // `parsedData` is the *parser's* flat view: the list of
            // argument patterns as written, plus the real (un-curried)
            // body. We project the flat view because (a) it round-trips
            // through our flat green-tree shape directly, and (b) the
            // FCS curried encoding loses argument-pattern shape
            // information (e.g. `fun (x, y) -> …` collapses the tuple
            // into compiler-generated bindings inside the `body` slot).
            //
            // `parsedData` is `Some _` for source-written `fun`-lambdas
            // and `None` for compiler-generated lambdas (e.g. the
            // method-shape rewrite at `SyntaxTreeOps.fs`). All inputs
            // the diff harness sees here are source-written, so we
            // panic loudly on `None` to surface any future shape
            // that's reached this arm without going through `fun`.
            let f = fields(v);
            if !matches!(case_name(&f[4]), "Some") {
                panic!(
                    "Phase 5.2: SynExpr.Lambda must carry parsedData=Some for source-written `fun`-lambdas; got {:?}",
                    f[4],
                );
            }
            // `parsedData = Some(parsedArgs, parsedBody)`. F# tuples
            // round-trip through `FSharp.SystemTextJson` as JSON arrays
            // (`[parsedArgs, parsedBody]`); index 0 is the SynPat list,
            // index 1 is the real body expression.
            let parsed = &fields(&f[4])[0];
            let parsed = parsed
                .as_array()
                .expect("SynExpr.Lambda parsedData tuple must be a JSON array");
            let arg_list = parsed[0]
                .as_array()
                .expect("SynExpr.Lambda parsedData arg list must be a JSON array");
            let args = arg_list.iter().map(fcs_pat).collect();
            NormalisedExpr::Lambda {
                args,
                body: Box::new(fcs_expr(&parsed[1])),
            }
        }
        "Match" => {
            // `SynExpr.Match of matchDebugPoint: DebugPointAtBinding *
            //                   expr: SynExpr * clauses: SynMatchClause list *
            //                   range: range * trivia: SynExprMatchTrivia`
            // (`SyntaxTree.fsi:847`). Field 1 is the scrutinee, field 2 the
            // clause list; the debug-point, range, and trivia slots are
            // elided. We only reach this arm through the synthetic
            // `fun`-parameter lowering (FCS's `SimplePatOfPat` match
            // scaffold) — a surface `match` expression isn't parsed yet.
            let f = fields(v);
            let clause_list = f[2]
                .as_array()
                .expect("SynExpr.Match clauses field must be a JSON array");
            NormalisedExpr::Match {
                // Canonicalise the scrutinee's generated `_arg<N>` index (see
                // `super::canonicalise_scrutinee`); the our-side projector does
                // the same at its `Match` construction.
                scrutinee: Box::new(super::canonicalise_scrutinee(fcs_expr(&f[1]))),
                clauses: clause_list.iter().map(fcs_match_clause).collect(),
            }
        }
        "MatchBang" => {
            // `SynExpr.MatchBang of matchDebugPoint: DebugPointAtBinding *
            //                       expr: SynExpr * clauses: SynMatchClause
            //                       list * range * trivia` (`SyntaxTree.fsi:916`)
            // — field-for-field identical to `Match` (field 1 scrutinee,
            // field 2 clauses; debug-point/range/trivia elided). The `match!`
            // computation-expression binder; FCS parses it at any expression
            // position. Kept a distinct `NormalisedExpr` variant so it never
            // matches a plain `match`.
            let f = fields(v);
            let clause_list = f[2]
                .as_array()
                .expect("SynExpr.MatchBang clauses field must be a JSON array");
            NormalisedExpr::MatchBang {
                scrutinee: Box::new(super::canonicalise_scrutinee(fcs_expr(&f[1]))),
                clauses: clause_list.iter().map(fcs_match_clause).collect(),
            }
        }
        "TryWith" => {
            // `SynExpr.TryWith of tryExpr: SynExpr * withCases: SynMatchClause
            //                    list * range * tryDebugPoint: DebugPointAtTry *
            //                    withDebugPoint: DebugPointAtWith * trivia`
            // (`SyntaxTree.fsi:759`). Field 0 is the protected body, field 1 the
            // handler clause list; the two debug points, range, and trivia slots
            // are elided. The clause list reuses `fcs_match_clause` (FCS's
            // `withCases` is the same `SynMatchClause list` as a `match`).
            let f = fields(v);
            let clause_list = f[1]
                .as_array()
                .expect("SynExpr.TryWith withCases field must be a JSON array");
            NormalisedExpr::TryWith {
                body: Box::new(fcs_expr(&f[0])),
                clauses: clause_list.iter().map(fcs_match_clause).collect(),
            }
        }
        "TryFinally" => {
            // `SynExpr.TryFinally of tryExpr: SynExpr * finallyExpr: SynExpr *
            //                       range * tryDebugPoint: DebugPointAtTry *
            //                       finallyDebugPoint: DebugPointAtFinally *
            //                       trivia` (`SyntaxTree.fsi:768`). Field 0 is the
            // protected body, field 1 the finally cleanup; the two debug points,
            // range, and trivia slots are elided.
            let f = fields(v);
            NormalisedExpr::TryFinally {
                body: Box::new(fcs_expr(&f[0])),
                finally: Box::new(fcs_expr(&f[1])),
            }
        }
        "While" => {
            // `SynExpr.While of whileDebugPoint: DebugPointAtWhile *
            //                   whileExpr: SynExpr * doExpr: SynExpr * range`
            // (`SyntaxTree.fsi:656`). Field 1 is the condition, field 2 the
            // body; the debug-point and range slots are elided.
            let f = fields(v);
            NormalisedExpr::While {
                cond: Box::new(fcs_expr(&f[1])),
                body: Box::new(fcs_expr(&f[2])),
            }
        }
        "WhileBang" => {
            // `SynExpr.WhileBang of whileDebugPoint: DebugPointAtWhile *
            //                       whileExpr: SynExpr * doExpr: SynExpr * range`
            // (`SyntaxTree.fsi:928`) — identical fields to `While` (field 1
            // condition, field 2 body). Kept a distinct `NormalisedExpr`
            // variant so it never matches a plain `while`.
            let f = fields(v);
            NormalisedExpr::WhileBang {
                cond: Box::new(fcs_expr(&f[1])),
                body: Box::new(fcs_expr(&f[2])),
            }
        }
        "ForEach" => {
            // `SynExpr.ForEach of forDebugPoint: DebugPointAtFor *
            //                    inDebugPoint: DebugPointAtInOrTo *
            //                    seqExprOnly: SeqExprOnly * isFromSource: bool *
            //                    pat: SynPat * enumExpr: SynExpr *
            //                    bodyExpr: SynExpr * range` (`SyntaxTree.fsi:671`).
            // Field 4 is the binder pattern, 5 the enumerable collection, 6 the
            // body; the two debug points, `seqExprOnly`, `isFromSource`, and
            // range are elided.
            let f = fields(v);
            NormalisedExpr::ForEach {
                pat: fcs_pat(&f[4]),
                enum_expr: Box::new(fcs_expr(&f[5])),
                body: Box::new(fcs_expr(&f[6])),
            }
        }
        "JoinIn" => {
            // `SynExpr.JoinIn of lhsExpr: SynExpr * lhsRange: range *
            //                   rhsExpr: SynExpr * range` (`SyntaxTree.fsi:883`)
            // — the query CE join operator `lhs in rhs`. Field 0 is the left
            // operand, field 2 the right; the `lhsRange` (field 1) and overall
            // range (field 3) are elided.
            let f = fields(v);
            NormalisedExpr::JoinIn {
                lhs: Box::new(fcs_expr(&f[0])),
                rhs: Box::new(fcs_expr(&f[2])),
            }
        }
        "For" => {
            // `SynExpr.For of forDebugPoint * toDebugPoint * ident: Ident *
            //                equalsRange: range option * identBody: SynExpr *
            //                direction: bool * toBody: SynExpr * doBody: SynExpr *
            //                range` (`SyntaxTree.fsi:659`). Field 2 is the loop
            // variable, 4 the start bound, 5 the direction (`true` = `to`), 6 the
            // end bound, 7 the body; the debug points, `equalsRange`, and range
            // are elided.
            let f = fields(v);
            NormalisedExpr::For {
                ident: f[2]
                    .get("idText")
                    .and_then(Value::as_str)
                    .expect("For ident has idText")
                    .to_string(),
                from: Box::new(fcs_expr(&f[4])),
                ascending: f[5].as_bool().expect("For direction is a bool"),
                to: Box::new(fcs_expr(&f[6])),
                body: Box::new(fcs_expr(&f[7])),
            }
        }
        "Quote" => {
            // `SynExpr.Quote of operator: SynExpr * isRaw: bool *
            //                  quotedExpr: SynExpr *
            //                  isFromQueryExpression: bool * range`
            // (`SyntaxTree.fsi:603`). Field 1 is `isRaw`, field 2 the quoted
            // expression. Field 0 (the synthetic `op_Quotation` operator
            // ident) and field 3 (`isFromQueryExpression`, always `false` at
            // parse) are elided.
            let f = fields(v);
            let is_raw = f[1]
                .as_bool()
                .expect("SynExpr.Quote field 1 (isRaw) must be a JSON bool");
            NormalisedExpr::Quote {
                is_raw,
                inner: Box::new(fcs_expr(&f[2])),
            }
        }
        "ComputationExpr" => {
            // `SynExpr.ComputationExpr of hasSeqBuilder: bool * expr: SynExpr *
            //                            range` (`SyntaxTree.fsi:702`). Field 1
            // is the brace body; `hasSeqBuilder` (field 0, always `false` at
            // parse) is elided.
            let f = fields(v);
            NormalisedExpr::ComputationExpr(Box::new(fcs_expr(&f[1])))
        }
        "Record" => {
            // `SynExpr.Record(baseInfo, copyInfo, recordFields, range)`
            // (`SyntaxTree.fsi:634`). Field 0 `baseInfo` (`inherit` records) is
            // deferred — asserted absent; field 1 `copyInfo` is the
            // `{ src with … }` source `(SynExpr * BlockSeparator) option`
            // (the `Some` wraps a `[expr, blockSep]` tuple); field 2 is the
            // `SynExprRecordField` list.
            let f = fields(v);
            // Field 0 `baseInfo` — `{ inherit Base(args); … }`. `Some` wraps a
            // `[SynType, SynExpr, …]` tuple: element 0 the base type, element 1 the
            // constructor-args expression (FCS synthesises `Const(Unit)` for a bare
            // `inherit Base` / `inherit Base()`). The remaining tuple elements
            // (paren ranges, block separator, range) are elided.
            let inherit_info = if f[0].is_null() {
                None
            } else {
                match case_name(&f[0]) {
                    "Some" => {
                        let tup = fields(&f[0])[0]
                            .as_array()
                            .expect("Record baseInfo Some wraps a tuple");
                        Some((fcs_type(&tup[0]), Box::new(fcs_expr(&tup[1]))))
                    }
                    "None" => None,
                    other => panic!("unexpected SynExpr.Record baseInfo Option case {other:?}"),
                }
            };
            let copy = if f[1].is_null() {
                None
            } else {
                match case_name(&f[1]) {
                    "Some" => {
                        let pair = fields(&f[1])[0]
                            .as_array()
                            .expect("Record copyInfo Some wraps a (expr, sep) tuple");
                        Some(Box::new(fcs_expr(&pair[0])))
                    }
                    "None" => None,
                    other => panic!("unexpected SynExpr.Record copyInfo Option case {other:?}"),
                }
            };
            let fields = f[2]
                .as_array()
                .expect("SynExpr.Record recordFields must be a JSON array")
                .iter()
                .map(fcs_record_field)
                .collect();
            NormalisedExpr::Record {
                inherit_info,
                copy,
                fields,
            }
        }
        "AnonRecd" => {
            // `SynExpr.AnonRecd(isStruct, copyInfo, recordFields, range,
            // trivia)` (`SyntaxTree.fsi:620`). Field 0 is `isStruct`; field 1
            // the `{| src with … |}` copy source (`(SynExpr * BlockSeparator)
            // option` — `Some` wraps a `[expr, sep]` tuple); field 2 the
            // `(SynLongIdent * range option * SynExpr) list` — each field is a
            // *3-element JSON array* (not a `SynExprRecordField`), so it can't
            // reuse `fcs_record_field`. The field value is mandatory (no Option).
            let f = fields(v);
            let is_struct = f[0]
                .as_bool()
                .expect("SynExpr.AnonRecd isStruct must be bool");
            let copy = if f[1].is_null() {
                None
            } else {
                match case_name(&f[1]) {
                    "Some" => {
                        let pair = fields(&f[1])[0]
                            .as_array()
                            .expect("AnonRecd copyInfo Some wraps a (expr, sep) tuple");
                        Some(Box::new(fcs_expr(&pair[0])))
                    }
                    "None" => None,
                    other => panic!("unexpected SynExpr.AnonRecd copyInfo Option case {other:?}"),
                }
            };
            let fields = f[2]
                .as_array()
                .expect("SynExpr.AnonRecd recordFields must be a JSON array")
                .iter()
                .map(|fld| {
                    let tuple = fld
                        .as_array()
                        .expect("AnonRecd field is a (SynLongIdent, mEquals, SynExpr) tuple");
                    NormalisedRecordField {
                        name: fcs_syn_long_ident_segments(&tuple[0]),
                        value: Some(Box::new(fcs_expr(&tuple[2]))),
                    }
                })
                .collect();
            NormalisedExpr::AnonRecd {
                is_struct,
                copy,
                fields,
            }
        }
        "ArrayOrList" => {
            // `SynExpr.ArrayOrList of isArray: bool * exprs: SynExpr list *
            //                         range` (`SyntaxTree.fsi:628`). Field 0 is
            // `isArray`, field 1 the element list. Range elided. The parser
            // emits this only for the empty body (`[]` / `[||]`); a non-empty
            // bracket is `ArrayOrListComputed`. We still decode `exprs` fully.
            let f = fields(v);
            let is_array = f[0]
                .as_bool()
                .expect("SynExpr.ArrayOrList field 0 is isArray bool");
            let exprs = f[1]
                .as_array()
                .expect("SynExpr.ArrayOrList field 1 is SynExpr list");
            let elements = exprs.iter().map(fcs_expr).collect();
            NormalisedExpr::ArrayOrList { is_array, elements }
        }
        "ArrayOrListComputed" => {
            // `SynExpr.ArrayOrListComputed of isArray: bool * expr: SynExpr *
            //                                 range` (`SyntaxTree.fsi:682`).
            // Field 0 is `isArray`, field 1 the single `sequentialExpr` body
            // (a right-leaning `Sequential` for two-or-more elements, flattened
            // by the `Sequential` arm). Range elided.
            let f = fields(v);
            let is_array = f[0]
                .as_bool()
                .expect("SynExpr.ArrayOrListComputed field 0 is isArray bool");
            NormalisedExpr::ArrayOrListComputed {
                is_array,
                inner: Box::new(fcs_expr(&f[1])),
            }
        }
        "YieldOrReturn" | "YieldOrReturnFrom" => {
            // `SynExpr.YieldOrReturn of flags: (bool * bool) * expr: SynExpr *
            //                          range * trivia` (`SyntaxTree.fsi:899`),
            // and the parallel `YieldOrReturnFrom` (`:904`). Field 0 is the
            // `(bool, bool)` flag tuple, field 1 the expression.
            let f = fields(v);
            let flag_arr = f[0]
                .as_array()
                .expect("YieldOrReturn(From) field 0 must be a [bool, bool] array");
            let flags = (
                flag_arr[0]
                    .as_bool()
                    .expect("YieldOrReturn flag 0 is a bool"),
                flag_arr[1]
                    .as_bool()
                    .expect("YieldOrReturn flag 1 is a bool"),
            );
            NormalisedExpr::YieldOrReturn {
                flags,
                from: case == "YieldOrReturnFrom",
                inner: Box::new(fcs_expr(&f[1])),
            }
        }
        "DoBang" => {
            // `SynExpr.DoBang of expr: SynExpr * range * trivia`
            // (`SyntaxTree.fsi:925`). Field 0 is the bound expression.
            let f = fields(v);
            NormalisedExpr::DoBang(Box::new(fcs_expr(&f[0])))
        }
        "Do" => {
            // `SynExpr.Do of expr: SynExpr * range` (`SyntaxTree.fsi:884`).
            // Field 0 is the bound expression.
            let f = fields(v);
            NormalisedExpr::Do(Box::new(fcs_expr(&f[0])))
        }
        "LetOrUse" => {
            // `SynExpr.LetOrUse of SynLetOrUse` (`SyntaxTree.fsi:913`). Field 0
            // is the `SynLetOrUse` record `{ IsRecursive, Bindings, Body, … }`.
            // `IsBang`/`IsUse` are computed members (not serialised) — each
            // binding's leading keyword is read by `fcs_binding` instead. `let!
            // … and! …` is one `LetOrUse` with several `Bindings`.
            let f = fields(v);
            let rec = &f[0];
            let is_rec = rec
                .get("IsRecursive")
                .and_then(Value::as_bool)
                .expect("SynLetOrUse.IsRecursive must be a JSON bool");
            let bindings = rec
                .get("Bindings")
                .and_then(Value::as_array)
                .expect("SynLetOrUse.Bindings must be a JSON array")
                .iter()
                .map(fcs_binding)
                .collect();
            let body = fcs_expr(rec.get("Body").expect("SynLetOrUse.Body"));
            NormalisedExpr::LetOrUse {
                is_rec,
                bindings,
                body: Box::new(body),
            }
        }
        "MatchLambda" => {
            // `SynExpr.MatchLambda of isExnMatch: bool *
            //                         keywordRange: range *
            //                         matchClauses: SynMatchClause list *
            //                         matchDebugPoint: DebugPointAtBinding *
            //                         range: range` (`SyntaxTree.fsi`). Field
            // 2 is the clause list; the exn-match flag, keyword range,
            // debug-point, and range slots are elided. Unlike `Match`, there
            // is no scrutinee field.
            let f = fields(v);
            let clause_list = f[2]
                .as_array()
                .expect("SynExpr.MatchLambda clauses field must be a JSON array");
            NormalisedExpr::MatchLambda {
                clauses: clause_list.iter().map(fcs_match_clause).collect(),
            }
        }
        "LongIdentSet" => {
            // `SynExpr.LongIdentSet of longDotId: SynLongIdent * expr: SynExpr *
            //                          range` (`SyntaxTree.fsi:819`) — `x <- e` /
            // `a.b.c <- e`, the `LongOrSingleIdent` arm of `mkSynAssign`. Field
            // 0 is the target path (a `SynLongIdent`), field 1 the assigned
            // value; the range is elided.
            let f = fields(v);
            NormalisedExpr::LongIdentSet {
                long_dot_id: fcs_syn_long_ident_segments(&f[0]),
                value: Box::new(fcs_expr(&f[1])),
            }
        }
        "Set" => {
            // `SynExpr.Set of targetExpr: SynExpr * rhsExpr: SynExpr * range`
            // (`SyntaxTree.fsi:832`) — the `mkSynAssign` fallback for a `<-`
            // whose LHS is neither an identifier path nor a recognised
            // indexed/property target. Field 0 is the LHS target, field 1 the
            // RHS value; the range is elided.
            let f = fields(v);
            NormalisedExpr::Set {
                target: Box::new(fcs_expr(&f[0])),
                value: Box::new(fcs_expr(&f[1])),
            }
        }
        "NamedIndexedPropertySet" => {
            // `SynExpr.NamedIndexedPropertySet of longDotId: SynLongIdent *
            //    expr1: SynExpr * expr2: SynExpr * range` (`SyntaxTree.fsi:847`)
            // — `Type.Items(e1) <- e2`. Field 0 is the function path, field 1
            // the index argument, field 2 the assigned value; range elided.
            let f = fields(v);
            NormalisedExpr::NamedIndexedPropertySet {
                long_dot_id: fcs_syn_long_ident_segments(&f[0]),
                expr1: Box::new(fcs_expr(&f[1])),
                expr2: Box::new(fcs_expr(&f[2])),
            }
        }
        "DotIndexedSet" => {
            // `SynExpr.DotIndexedSet of objectExpr: SynExpr * indexArgs: SynExpr
            //    * valueExpr: SynExpr * leftOfSetRange * dotRange * range`
            // (`SyntaxTree.fsi:838`) — `arr.[i] <- v`, the `mkSynAssign` arm for
            // a `DotIndexedGet` LHS. Field 0 is the object, field 1 the index
            // args, field 2 the assigned value; the three ranges are elided.
            let f = fields(v);
            NormalisedExpr::DotIndexedSet {
                object: Box::new(fcs_expr(&f[0])),
                index: Box::new(fcs_expr(&f[1])),
                value: Box::new(fcs_expr(&f[2])),
            }
        }
        "DotSet" => {
            // `SynExpr.DotSet of targetExpr: SynExpr * longDotId: SynLongIdent *
            //    rhsExpr: SynExpr * range` (`SyntaxTree.fsi:828`) —
            // `expr.Member <- v`, the `mkSynAssign` arm for a `DotGet` LHS.
            // Field 0 is the object, field 1 the member path, field 2 the
            // assigned value; the range is elided.
            let f = fields(v);
            NormalisedExpr::DotSet {
                expr: Box::new(fcs_expr(&f[0])),
                long_dot_id: fcs_syn_long_ident_segments(&f[1]),
                value: Box::new(fcs_expr(&f[2])),
            }
        }
        "DotNamedIndexedPropertySet" => {
            // `SynExpr.DotNamedIndexedPropertySet of targetExpr: SynExpr *
            //    longDotId: SynLongIdent * argExpr: SynExpr * rhsExpr: SynExpr *
            //    range` (`SyntaxTree.fsi:850`) — `expr.Member(i) <- v`, the
            // `mkSynAssign` arm for an `App(DotGet, x)` LHS. Field 0 is the
            // receiver object, field 1 the member path, field 2 the index
            // argument, field 3 the assigned value; the range is elided.
            let f = fields(v);
            NormalisedExpr::DotNamedIndexedPropertySet {
                target: Box::new(fcs_expr(&f[0])),
                long_dot_id: fcs_syn_long_ident_segments(&f[1]),
                expr1: Box::new(fcs_expr(&f[2])),
                expr2: Box::new(fcs_expr(&f[3])),
            }
        }
        "ArbitraryAfterError" => {
            // `SynExpr.ArbitraryAfterError of debugStr: string * range`
            // (`SyntaxTree.fsi`) — FCS's error-recovery placeholder for a
            // missing/unparseable expression (e.g. the RHS of `let x =` with
            // nothing after the `=`). Shape-only marker; `debugStr`/range are
            // elided. Our side projects the equivalent recovery hole (an absent
            // `Expr` child under a recovered binding) to the same variant.
            NormalisedExpr::Error
        }
        "LibraryOnlyStaticOptimization" => {
            // `SynExpr.LibraryOnlyStaticOptimization(constraints:
            // SynStaticOptimizationConstraint list, expr: SynExpr, optimizedExpr:
            // SynExpr, range)` (`SyntaxTree.fsi:939`) — FSharp.Core's
            // static-optimization binding RHS, the nested fold
            // `SyntaxTreeOps.mkSynBindingRhs` builds. Field 0 is the outermost
            // clause's condition list, field 1 its branch, field 2 the rest (the
            // next nested optimization, bottoming out at the fallthrough main
            // expression). Unlike inline IL, every field is serialisable, so this
            // is fully modelled; the CST projector reproduces the same nesting.
            let f = fields(v);
            // FCS's `staticOptimizationConditions` grammar is left-recursive and
            // *prepends* (`$3 :: $1`) without a final `List.rev`, so the `and`-
            // chained conditions arrive in reverse source order. The conjunction
            // is order-independent; normalise to source order (matching the CST
            // projector's left-to-right `conditions()`) by reversing here.
            let constraints = f[0]
                .as_array()
                .expect("SynExpr.LibraryOnlyStaticOptimization field 0 must be a constraint array")
                .iter()
                .rev()
                .map(fcs_static_opt_constraint)
                .collect();
            NormalisedExpr::StaticOptimization {
                constraints,
                expr: Box::new(fcs_expr(&f[1])),
                optimized_expr: Box::new(fcs_expr(&f[2])),
            }
        }
        "LibraryOnlyUnionCaseFieldGet" => {
            // `LibraryOnlyUnionCaseFieldGet(expr: SynExpr, longId: LongIdent,
            // fieldNum: int, range)` — FSharp.Core's `expr.( :: ).<int>`. Field 0
            // the object, field 2 the field number; the `longId` (field 1) is
            // always `["op_ColonColon"]` (grammar-fixed), so it is elided.
            let f = fields(v);
            NormalisedExpr::LibraryOnlyUnionCaseFieldGet {
                expr: Box::new(fcs_expr(&f[0])),
                field_num: fcs_field_num(&f[2]),
            }
        }
        "LibraryOnlyUnionCaseFieldSet" => {
            // `LibraryOnlyUnionCaseFieldSet(expr, longId, fieldNum, rhsExpr,
            // range)` — the set form `expr.( :: ).<int> <- rhs`, which FCS's
            // `mkSynAssign` builds from the get. Field 0 the object, 2 the field
            // number, 3 the assigned value; `longId` (1) elided as above.
            let f = fields(v);
            NormalisedExpr::LibraryOnlyUnionCaseFieldSet {
                expr: Box::new(fcs_expr(&f[0])),
                field_num: fcs_field_num(&f[2]),
                value: Box::new(fcs_expr(&f[3])),
            }
        }
        other => panic!("Phase 2: unsupported SynExpr case {other:?}"),
    }
}

/// Project a JSON-encoded `SynMatchClause of pat: SynPat *
/// whenExpr: SynExpr option * resultExpr: SynExpr * range: range *
/// debugPoint: DebugPointAtTarget * trivia: SynMatchClauseTrivia`
/// (`SyntaxTree.fsi:1063`). Field 0 is the clause pattern, field 1 the
/// optional `when` guard (`None` → plain JSON `null`, `Some` → the
/// AdjacentTag `{Case: "Some", Fields: [<expr>]}` form, matching the
/// option convention used for `IfThenElse`), field 2 the result
/// expression; range, debug-point, and trivia are elided.
fn fcs_match_clause(v: &Value) -> NormalisedMatchClause {
    let f = fields(v);
    let when = if f[1].is_null() {
        None
    } else {
        match case_name(&f[1]) {
            "Some" => Some(Box::new(fcs_expr(&fields(&f[1])[0]))),
            "None" => None,
            other => panic!("unexpected SynMatchClause whenExpr Option case {other:?}"),
        }
    };
    NormalisedMatchClause {
        pat: fcs_pat(&f[0]),
        when,
        result: Box::new(fcs_expr(&f[2])),
    }
}

/// Project a JSON-encoded `SynExprRecordField(fieldName, equalsRange, expr,
/// range, blockSeparator)` (`SyntaxTree.fsi:991`). Field 0 is the
/// `RecordFieldName = (SynLongIdent * bool)` tuple (we take the `SynLongIdent`
/// segments, dropping the trailing-dot bool); field 2 is the `SynExpr option`
/// value; the equals/separator/range slots are elided.
fn fcs_record_field(v: &Value) -> NormalisedRecordField {
    let f = fields(v);
    let field_name = f[0]
        .as_array()
        .expect("RecordFieldName is a (SynLongIdent, bool) tuple");
    let name = fcs_syn_long_ident_segments(&field_name[0]);
    let value = if f[2].is_null() {
        None
    } else {
        match case_name(&f[2]) {
            "Some" => Some(Box::new(fcs_expr(&fields(&f[2])[0]))),
            "None" => None,
            other => panic!("unexpected SynExprRecordField expr Option case {other:?}"),
        }
    };
    NormalisedRecordField { name, value }
}

/// Project a JSON-encoded `SynInterpolatedStringPart`. The two cases
/// (`String of value * range`, `FillExpr of fillExpr * qualifiers: Ident
/// option`) ride through as `AdjacentTag` records; we project the value text
/// (for String) or the inner expression plus its qualifier (for FillExpr),
/// eliding only ranges. The `Ident option` qualifier serialises with a
/// mixed encoding: `None` is bare `null`, while `Some` is a `{Case: "Some",
/// Fields: [ident]}` DU whose payload is the `Ident` record (`{ idText,
/// idRange }`), whose `idText` we keep.
fn fcs_interp_part(v: &Value) -> NormalisedInterpPart {
    let case = case_name(v);
    let f = fields(v);
    match case {
        "String" => {
            let value = fcs_utf16_units(&f[0], "SynInterpolatedStringPart.String field 0 (value)");
            NormalisedInterpPart::String(value)
        }
        "FillExpr" => {
            let expr = fcs_expr(&f[0]);
            let qualifier = if f[1].is_null() {
                None
            } else {
                match case_name(&f[1]) {
                    "Some" => Some(
                        fields(&f[1])[0]
                            .get("idText")
                            .and_then(Value::as_str)
                            .expect("SynInterpolatedStringPart.FillExpr qualifier Ident has idText")
                            .to_string(),
                    ),
                    other => panic!("unexpected FillExpr qualifiers Option case {other:?}"),
                }
            };
            NormalisedInterpPart::FillExpr { expr, qualifier }
        }
        other => panic!("unknown SynInterpolatedStringPart case {other:?}"),
    }
}

/// `SynStringKind` discriminator: `{Case: "Regular" | "Verbatim" |
/// "TripleQuote"}`. Lifts the AdjacentTag case to the
/// [`SynStringKind`] enum and panics on unknown variants so a future
/// addition surfaces clearly.
fn fcs_syn_string_kind(v: &Value) -> SynStringKind {
    match case_name(v) {
        "Regular" => SynStringKind::Regular,
        "Verbatim" => SynStringKind::Verbatim,
        "TripleQuote" => SynStringKind::TripleQuote,
        other => panic!("unknown SynStringKind case {other:?}"),
    }
}

/// Project an FCS `SynType` JSON value to [`NormalisedType`]. Phases
/// 7.1–7.9 model the atomic shapes, type variables, function arrows,
/// tuple types, postfix/prefix application, array suffixes, hash
/// constraints, and anon-record types; anything else panics so the
/// harness loudly flags new variants the projector hasn't grown to
/// cover.
fn fcs_type(v: &Value) -> NormalisedType {
    let case = case_name(v);
    match case {
        "LongIdent" => {
            // `SynType.LongIdent of longDotId: SynLongIdent`. Field 0 is
            // the SynLongIdent. Same projection as `SynExpr.LongIdent`.
            let f = fields(v);
            NormalisedType::LongIdent(fcs_syn_long_ident_segments(&f[0]))
        }
        "Anon" => {
            // `SynType.Anon of range`. The range is elided; payload-less
            // variant matches.
            NormalisedType::Anon
        }
        "Paren" => {
            // `SynType.Paren of innerType: SynType * range: range`
            // (`SyntaxTree.fsi:530`). Field 0 is the wrapped type.
            let f = fields(v);
            NormalisedType::Paren(Box::new(fcs_type(&f[0])))
        }
        "Var" => {
            // `SynType.Var of typar: SynTypar * range: range`
            // (`SyntaxTree.fsi:509`). Field 0 is the `SynTypar`, itself a
            // single-case DU `SynTypar of ident * staticReq * isCompGen`
            // (`SyntaxTree.fsi:87`). The static-req discriminant is its
            // own payload-less DU; we read the case tag for `HeadType` vs
            // `None`.
            let f = fields(v);
            let typar = &f[0];
            let typar_fields = self::fields(typar);
            let name = typar_fields[0]
                .get("idText")
                .and_then(Value::as_str)
                .expect("SynTypar Ident record has idText")
                .to_string();
            let head_type = match case_name(&typar_fields[1]) {
                "None" => false,
                "HeadType" => true,
                other => panic!("unknown TyparStaticReq case {other:?}"),
            };
            NormalisedType::Var { name, head_type }
        }
        "Fun" => {
            // `SynType.Fun of argType: SynType * returnType: SynType *
            //                 range: range * trivia: SynTypeFunTrivia`
            // (`SyntaxTree.fsi:506`). Fields 0/1 are the argument and
            // return types; fields 2 (range) and 3 (trivia carrying
            // `ArrowRange`) are elided.
            let f = fields(v);
            NormalisedType::Fun {
                arg: Box::new(fcs_type(&f[0])),
                ret: Box::new(fcs_type(&f[1])),
            }
        }
        "Tuple" => {
            // `SynType.Tuple of isStruct: bool * path: SynTupleTypeSegment list *
            //                   range: range` (`SyntaxTree.fsi:496`). Fields 0
            // (isStruct), 1 (path), 2 (range elided). Each path element is
            // itself a single-case DU `SynTupleTypeSegment`.
            let f = fields(v);
            let is_struct = f[0].as_bool().expect("SynType.Tuple isStruct must be bool");
            let arr = f[1]
                .as_array()
                .expect("SynType.Tuple path must be an array");
            let path = arr.iter().map(fcs_tuple_segment).collect();
            NormalisedType::Tuple { is_struct, path }
        }
        "App" => {
            // `SynType.App of typeName: SynType * lessRange: range option *
            //                 typeArgs: SynType list * commaRanges: range list *
            //                 greaterRange: range option * isPostfix: bool *
            //                 range: range` (`SyntaxTree.fsi:472`). We keep
            // fields 0 (typeName), 2 (typeArgs), 5 (isPostfix); the
            // less/greater/comma ranges and outer range are elided.
            let f = fields(v);
            let type_name = Box::new(fcs_type(&f[0]));
            let type_args = f[2]
                .as_array()
                .expect("SynType.App typeArgs must be an array")
                .iter()
                .map(fcs_type)
                .collect();
            let is_postfix = f[5].as_bool().expect("SynType.App isPostfix must be bool");
            NormalisedType::App {
                type_name,
                type_args,
                is_postfix,
            }
        }
        "Array" => {
            // `SynType.Array of rank: int * elementType: SynType * range:
            // range` (`SyntaxTree.fsi:475`). Field 0 is the rank (1–32 in
            // practice), field 1 the element type; the range is elided.
            let f = fields(v);
            let rank = f[0]
                .as_u64()
                .expect("SynType.Array rank must be a non-negative integer")
                .try_into()
                .expect("SynType.Array rank fits in usize");
            let element_type = Box::new(fcs_type(&f[1]));
            NormalisedType::Array { rank, element_type }
        }
        "HashConstraint" => {
            // `SynType.HashConstraint of innerType: SynType * range:
            // range` (`SyntaxTree.fsi:518`). Single inner-type field; the
            // range is elided.
            let f = fields(v);
            let inner = Box::new(fcs_type(&f[0]));
            NormalisedType::Hash { inner }
        }
        "AnonRecd" => {
            // `SynType.AnonRecd of isStruct: bool * fields: (Ident *
            //                      SynType) list * range: range`
            // (`SyntaxTree.fsi:500`). Field 0 is the struct flag, field
            // 1 the (Ident, SynType) pair list, field 2 the range
            // (elided). Each pair is serialised as a JSON array of two
            // elements.
            let f = fields(v);
            let is_struct = f[0]
                .as_bool()
                .expect("SynType.AnonRecd isStruct must be bool");
            let arr = f[1]
                .as_array()
                .expect("SynType.AnonRecd fields must be an array");
            let fields_out = arr
                .iter()
                .map(|pair| {
                    let pa = pair
                        .as_array()
                        .expect("SynType.AnonRecd field pair must be a JSON array");
                    let ident = pa[0]
                        .get("idText")
                        .and_then(Value::as_str)
                        .expect("SynType.AnonRecd field ident has idText")
                        .to_string();
                    let ty = fcs_type(&pa[1]);
                    (ident, ty)
                })
                .collect();
            NormalisedType::AnonRecd {
                is_struct,
                fields: fields_out,
            }
        }
        "LongIdentApp" => {
            // `SynType.LongIdentApp of typeName: SynType *
            //                          longDotId: SynLongIdent *
            //                          lessRange: range option *
            //                          typeArgs: SynType list *
            //                          commaRanges: range list *
            //                          greaterRange: range option *
            //                          range: range`
            // (`SyntaxTree.fsi:452`). We keep fields 0 (typeName / root),
            // 1 (longDotId / path), 3 (typeArgs); the less / greater
            // / comma ranges and outer range are elided. SynLongIdent
            // projection mirrors `SynType.LongIdent` — original notation
            // for operator-form idents, falling back to `idText`
            // (which already strips backticks).
            let f = fields(v);
            let root = Box::new(fcs_type(&f[0]));
            let path: Vec<String> = fcs_syn_long_ident_segments(&f[1]);
            let type_args = f[3]
                .as_array()
                .expect("SynType.LongIdentApp typeArgs must be an array")
                .iter()
                .map(fcs_type)
                .collect();
            NormalisedType::LongIdentApp {
                root,
                path,
                type_args,
            }
        }
        "WithNull" => {
            // `SynType.WithNull of innerType: SynType * ambivalent: bool *
            //  range: range * trivia: SynTypeWithNullTrivia` (`SyntaxTree.fsi:536`).
            // Field 0 is the inner type; field 1 (ambivalent, always false at
            // parse), field 2 (range), and field 3 (BarRange trivia) are elided.
            let f = fields(v);
            NormalisedType::WithNull {
                inner: Box::new(fcs_type(&f[0])),
            }
        }
        "WithGlobalConstraints" => {
            // `SynType.WithGlobalConstraints of typeName: SynType *
            //  constraints: SynTypeConstraint list * range` — a type with a
            //  trailing `when` clause (`typeWithTypeConstraints`). Field 0 is the
            //  base type; field 1 the `and`-separated constraint list (reusing
            //  the shared `fcs_type_constraint` decoder); the range is elided.
            let f = fields(v);
            let constraints = f[1]
                .as_array()
                .expect("SynType.WithGlobalConstraints constraints must be an array")
                .iter()
                .map(fcs_type_constraint)
                .collect();
            NormalisedType::WithGlobalConstraints {
                base: Box::new(fcs_type(&f[0])),
                constraints,
            }
        }
        "Intersection" => {
            // `SynType.Intersection of typar: SynTypar option *
            //  types: SynType list * range * trivia` (`SyntaxTree.fsi:557`,
            //  phase 10.10). Field 0 is the optional head typar — JSON `null`
            //  for `None` (the `#A & …` form) or `{Case:"Some", Fields:[SynTypar]}`
            //  for the `'T & …` form; field 1 is the `&`-separated operand list.
            //  The range and `AmpersandRanges` trivia (fields 2/3) are elided.
            let f = fields(v);
            let typar = if f[0].is_null() {
                None
            } else {
                Some(fcs_syntypar(&fields(&f[0])[0]))
            };
            let types = f[1]
                .as_array()
                .expect("SynType.Intersection types must be an array")
                .iter()
                .map(fcs_type)
                .collect();
            NormalisedType::Intersection { typar, types }
        }
        "MeasurePower" => {
            // `SynType.MeasurePower of baseMeasure: SynType * exponent:
            //  SynRationalConst * range: range` (`SyntaxTree.fsi:521`, phase
            // 10.8). Field 0 is the base measure, field 1 the rational-const
            // exponent; the range is elided.
            let f = fields(v);
            NormalisedType::MeasurePower {
                base: Box::new(fcs_type(&f[0])),
                exponent: fcs_rational_const(&f[1]),
            }
        }
        "StaticConstant" => {
            // `SynType.StaticConstant of constant: SynConst * range`
            // (`SyntaxTree.fsi:525`, phase 10.9). Field 0 is the `SynConst`;
            // the range is elided.
            let f = fields(v);
            NormalisedType::StaticConstant(fcs_const(&f[0]))
        }
        "StaticConstantExpr" => {
            // `SynType.StaticConstantExpr of expr: SynExpr * range`
            // (`SyntaxTree.fsi:531`, phase 10.9). Field 0 is the atomic
            // expression; the range is elided.
            let f = fields(v);
            NormalisedType::StaticConstantExpr(Box::new(fcs_expr(&f[0])))
        }
        "StaticConstantNamed" => {
            // `SynType.StaticConstantNamed of ident: SynType * value: SynType *
            //  range` (`SyntaxTree.fsi:534`, phase 10.9). Fields 0/1 are the
            // name and value types; the range is elided.
            let f = fields(v);
            NormalisedType::StaticConstantNamed {
                ident: Box::new(fcs_type(&f[0])),
                value: Box::new(fcs_type(&f[1])),
            }
        }
        "StaticConstantNull" => {
            // `SynType.StaticConstantNull of range` (`SyntaxTree.fsi:528`,
            // phase 10.9). Payload-less after eliding the range.
            NormalisedType::StaticConstantNull
        }
        "SignatureParameter" => {
            // `SynType.SignatureParameter(attributes, isOptional, id: Ident option,
            // usedType, range)` (phase 10.12b). Field 0 the attribute lists, 1 the
            // `isOptional` bool, 2 the optional parameter name, 3 the value type;
            // the range (4) and the `SynArgInfo` companion are elided.
            let f = fields(v);
            let attributes = fcs_attribute_lists(&f[0]);
            let is_optional = f[1]
                .as_bool()
                .expect("SignatureParameter isOptional is a bool");
            let id = match &f[2] {
                Value::Null => None,
                some => Some(
                    fields(some)[0]
                        .get("idText")
                        .and_then(Value::as_str)
                        .expect("SignatureParameter id has idText")
                        .to_string(),
                ),
            };
            NormalisedType::SignatureParameter {
                attributes,
                is_optional,
                id,
                used_type: Box::new(fcs_type(&f[3])),
            }
        }
        other => panic!("Phase 10.9: unsupported SynType case {other:?}"),
    }
}

/// Project an FCS `SynRationalConst` JSON value to [`NormalisedRationalConst`]
/// (`SyntaxTree.fsi:221-235`, phase 10.8). All ranges are elided.
fn fcs_rational_const(v: &Value) -> NormalisedRationalConst {
    let case = case_name(v);
    let f = fields(v);
    match case {
        // `Integer of value: int32 * range`.
        "Integer" => NormalisedRationalConst::Integer(fcs_rational_int(&f[0])),
        // `Rational of numerator: int32 * numeratorRange * divRange *
        //  denominator: int32 * denominatorRange * range`. Numerator is
        // field 0, denominator field 3; the three ranges are elided.
        "Rational" => NormalisedRationalConst::Rational {
            num: fcs_rational_int(&f[0]),
            denom: fcs_rational_int(&f[3]),
        },
        // `Negate of SynRationalConst * range`.
        "Negate" => NormalisedRationalConst::Negate(Box::new(fcs_rational_const(&f[0]))),
        // `Paren of SynRationalConst * range`.
        "Paren" => NormalisedRationalConst::Paren(Box::new(fcs_rational_const(&f[0]))),
        other => panic!("unknown SynRationalConst case {other:?}"),
    }
}

/// Read a `SynRationalConst` numerator / integer field as `i32`.
fn fcs_rational_int(v: &Value) -> i32 {
    v.as_i64()
        .expect("SynRationalConst integer field is an integer")
        .try_into()
        .expect("SynRationalConst integer value fits in i32")
}

/// Project an FCS `SynTupleTypeSegment` JSON value to a
/// [`NormalisedTupleSegment`]. One-for-one with the FCS cases; ranges
/// on the `Star`/`Slash` variants are elided so trivia drift can't
/// cause diff failures.
fn fcs_tuple_segment(v: &Value) -> NormalisedTupleSegment {
    let case = case_name(v);
    match case {
        "Type" => {
            let f = fields(v);
            NormalisedTupleSegment::Type(fcs_type(&f[0]))
        }
        "Star" => NormalisedTupleSegment::Star,
        "Slash" => NormalisedTupleSegment::Slash,
        other => panic!("unknown SynTupleTypeSegment case {other:?}"),
    }
}

fn fcs_const(v: &Value) -> NormalisedConst {
    let case = case_name(v);
    match case {
        "Int32" => {
            let fields = fields(v);
            let n = fields[0]
                .as_i64()
                .expect("SynConst.Int32 field is integer")
                .try_into()
                .expect("SynConst.Int32 value fits in i32");
            NormalisedConst::Int32(n)
        }
        // Signed types can be negative when the source uses a hex/oct/bin
        // bit pattern with the top bit set (`0x80y` = -128, `0xFFFFL` =
        // 65535 here is unsigned but `0xFFFFFFFFFFFFFFFFL` = -1, etc.).
        // The lexer's two's-complement narrowing happens at lex time, so
        // FCS hands us a negative `int8`/`int16`/`int64`/`int64`-as-IntPtr
        // and System.Text.Json serialises that as a negative JSON number.
        // Read via `as_i64` (covers every signed width) and narrow.
        "SByte" => NormalisedConst::SByte(fcs_typed_signed(v, "SByte")),
        "Int16" => NormalisedConst::Int16(fcs_typed_signed(v, "Int16")),
        "Int64" => NormalisedConst::Int64(fcs_typed_signed(v, "Int64")),
        // `SynConst.IntPtr` carries an `int64`; same channel as Int64.
        "IntPtr" => NormalisedConst::IntPtr(fcs_typed_signed(v, "IntPtr")),
        // Unsigned types are always non-negative in the JSON.
        "Byte" => NormalisedConst::Byte(fcs_typed_unsigned(v, "Byte")),
        "UInt16" => NormalisedConst::UInt16(fcs_typed_unsigned(v, "UInt16")),
        "UInt32" => NormalisedConst::UInt32(fcs_typed_unsigned(v, "UInt32")),
        "UInt64" => NormalisedConst::UInt64(fcs_typed_unsigned(v, "UInt64")),
        "UIntPtr" => NormalisedConst::UIntPtr(fcs_typed_unsigned(v, "UIntPtr")),
        // `SynConst.Double` carries a `double` (64-bit IEEE). `fcs-dump`'s
        // `DoubleConverter` emits the exact `DoubleToInt64Bits` pattern as a
        // JSON integer rather than float text: `serde_json`'s float parser is
        // not always correctly rounded (e.g. `1.5430806348152437` decodes one
        // ULP low), so routing the value through `.as_f64()` would spuriously
        // diverge from our own correctly-rounded parse. Reading the integer
        // bits is exact and also preserves signed-zero / NaN payloads.
        "Double" => {
            let bits = fields(v)[0]
                .as_i64()
                .expect("SynConst.Double field is the i64 bit pattern");
            NormalisedConst::Double(bits as u64)
        }
        // `SynConst.Single` carries a `float32`; `fcs-dump`'s `SingleConverter`
        // emits the exact `SingleToInt32Bits` pattern as a JSON integer (same
        // rationale as `Double`).
        "Single" => {
            let bits = fields(v)[0]
                .as_i64()
                .expect("SynConst.Single field is the i32 bit pattern");
            NormalisedConst::Single(bits as i32 as u32)
        }
        // `SynConst.Char` carries a .NET `char` (one UTF-16 code unit).
        // `fcs-dump`'s `CharConverter` emits the raw unit as a JSON integer so
        // lone-surrogate recovery values do not collapse to U+FFFD.
        "Char" => {
            let unit = fields(v)[0]
                .as_u64()
                .expect("SynConst.Char field is the UTF-16 code unit");
            let unit = u16::try_from(unit).unwrap_or_else(|e| {
                panic!("SynConst.Char code unit {unit} does not fit u16: {e:?}")
            });
            NormalisedConst::Char(unit)
        }
        // `SynConst.String(text, kind, range)` — JSON shape:
        // `[[utf16-code-units], {Case: "Regular"|"Verbatim"|"TripleQuote"},
        // <range>]`. Range is elided; the decoded text and string-kind survive.
        "String" => {
            let f = fields(v);
            let value = fcs_utf16_units(&f[0], "SynConst.String field 0 (text)");
            let kind = match case_name(&f[1]) {
                "Regular" => SynStringKind::Regular,
                "Verbatim" => SynStringKind::Verbatim,
                "TripleQuote" => SynStringKind::TripleQuote,
                other => panic!("unknown SynStringKind case {other:?}"),
            };
            NormalisedConst::String { value, kind }
        }
        // `SynConst.Bytes(bytes, kind, range)` — JSON shape:
        // `[<base64-string>, {Case: "Regular"|"Verbatim"}, <range>]`.
        // System.Text.Json serialises `byte[]` as a standard RFC-4648
        // base64 string. Decode it back to the raw bytes for diff.
        "Bytes" => {
            let f = fields(v);
            let b64 = f[0]
                .as_str()
                .expect("SynConst.Bytes payload is a JSON string (System.Text.Json base64)");
            let value = decode_base64(b64);
            let kind = match case_name(&f[1]) {
                "Regular" => SynByteStringKind::Regular,
                "Verbatim" => SynByteStringKind::Verbatim,
                other => panic!("unknown SynByteStringKind case {other:?}"),
            };
            NormalisedConst::Bytes { value, kind }
        }
        // `SynConst.UserNum(value, suffix)` — both fields are JSON strings.
        "UserNum" => {
            let f = fields(v);
            let value = f[0]
                .as_str()
                .expect("SynConst.UserNum value is a JSON string")
                .to_string();
            let suffix = f[1]
                .as_str()
                .expect("SynConst.UserNum suffix is a JSON string")
                .to_string();
            NormalisedConst::UserNum { value, suffix }
        }
        // `SynConst.Decimal` carries a `System.Decimal`. The dump's
        // `DecimalConverter` writes it as a JSON string using
        // `decimal.ToString(InvariantCulture)`, which preserves the
        // trailing-zero scale (`1.0m` → `"1.0"`, `1.00m` → `"1.00"`). The
        // our-side projector canonicalises source text to that same shape,
        // so a string compare is the diff.
        "Decimal" => {
            let s = fields(v)[0]
                .as_str()
                .expect("SynConst.Decimal field is a JSON string (DecimalConverter)")
                .to_string();
            NormalisedConst::Decimal(s)
        }
        "Bool" => {
            let fields = fields(v);
            let b = fields[0].as_bool().expect("SynConst.Bool field is boolean");
            NormalisedConst::Bool(b)
        }
        // `SynConst.Unit` carries no fields in the FCS DU (just the case
        // tag); `fields(v)` would still return `[]`, but we don't bother
        // reading.
        "Unit" => NormalisedConst::Unit,
        // `SynConst.SourceIdentifier(constant, value, range)` — JSON shape:
        // `["__SOURCE_DIRECTORY__"|…, <expanded-value>, <range>]`. Field 0 is
        // the source spelling; field 1 is FCS's expanded value, validated here
        // before path-valued forms are canonicalised.
        "SourceIdentifier" => {
            let f = fields(v);
            let constant = f[0]
                .as_str()
                .expect("SynConst.SourceIdentifier constant field is a JSON string")
                .to_string();
            let value = fcs_source_identifier_value(
                &constant,
                &f[1],
                &f[2],
                "SynConst.SourceIdentifier value",
            );
            NormalisedConst::SourceIdentifier { constant, value }
        }
        // `SynConst.Measure(constant, constantRange, synMeasure, trivia)`. Field
        // 0 is the underlying numeric `SynConst`; field 2 is the `SynMeasure`.
        "Measure" => {
            let f = fields(v);
            NormalisedConst::Measure {
                constant: Box::new(fcs_const(&f[0])),
                measure: fcs_measure(&f[2]),
            }
        }
        other => panic!("Phase 2: unsupported SynConst case {other:?}"),
    }
}

/// Project an FCS `SynMeasure` JSON value to [`NormalisedMeasure`]. Mirrors the
/// `measureTypeExpr` grammar; ranges and operator ranges are elided.
fn fcs_measure(v: &Value) -> NormalisedMeasure {
    let case = case_name(v);
    let f = fields(v);
    match case {
        // `Named(longId: LongIdent, range)` — field 0 is a plain `Ident list`
        // (not a `SynLongIdent`), so reuse the shared ident-list reader, which
        // strips the `` `global` `` mangling FCS gives a `GLOBAL` path head to
        // line up with our bare `global`.
        "Named" => NormalisedMeasure::Named(fcs_ident_list_texts(&f[0])),
        // `Var(typar: SynTypar, range)`.
        "Var" => NormalisedMeasure::Var(fcs_syntypar(&f[0])),
        // `One(range)` / `Anon(range)` — no payload of interest.
        "One" => NormalisedMeasure::One,
        "Anon" => NormalisedMeasure::Anon,
        // `Seq(measures: SynMeasure list, range)` — field 0 is the list.
        "Seq" => NormalisedMeasure::Seq(
            f[0].as_array()
                .expect("SynMeasure.Seq measures is an array")
                .iter()
                .map(fcs_measure)
                .collect(),
        ),
        // `Product(measure1, mAsterisk, measure2, range)` — fields 0 and 2.
        "Product" => {
            NormalisedMeasure::Product(Box::new(fcs_measure(&f[0])), Box::new(fcs_measure(&f[2])))
        }
        // `Divide(measure1: SynMeasure option, mSlash, measure2, range)` — field
        // 0 is the optional numerator (`None` for the reciprocal `/s`), field 2
        // the denominator.
        "Divide" => {
            // `measure1` is `SynMeasure option` — `null` (None, the reciprocal
            // `/s`) or `{Case:"Some", Fields:[inner]}`.
            let numerator = if f[0].is_null() {
                None
            } else {
                match case_name(&f[0]) {
                    "Some" => Some(Box::new(fcs_measure(&fields(&f[0])[0]))),
                    "None" => None,
                    other => panic!("unexpected SynMeasure.Divide measure1 Option case {other:?}"),
                }
            };
            NormalisedMeasure::Divide(numerator, Box::new(fcs_measure(&f[2])))
        }
        // `Power(measure, caretRange, power: SynRationalConst, range)` — field 0
        // the base, field 2 the exponent.
        "Power" => {
            NormalisedMeasure::Power(Box::new(fcs_measure(&f[0])), fcs_rational_const(&f[2]))
        }
        // `Paren(measure, range)`.
        "Paren" => NormalisedMeasure::Paren(Box::new(fcs_measure(&f[0]))),
        other => panic!("unsupported SynMeasure case {other:?}"),
    }
}

/// Read field 0 of an unsigned `SynConst.<case>` JSON value as `u64`,
/// then narrow to `T`. Unsigned variants never appear as negative JSON
/// numbers; FCS would have errored at lex time first.
fn fcs_typed_unsigned<T: TryFrom<u64>>(v: &Value, case: &str) -> T
where
    <T as TryFrom<u64>>::Error: std::fmt::Debug,
{
    let n = fields(v)[0].as_u64().unwrap_or_else(|| {
        panic!("SynConst.{case} field 0 must be a non-negative integer JSON number")
    });
    T::try_from(n)
        .unwrap_or_else(|e| panic!("SynConst.{case} value {n} doesn't fit target type: {e:?}"))
}

/// Read field 0 of a signed `SynConst.<case>` JSON value as `i64`,
/// then narrow to `T`. The signed forms hit negative values when a
/// hex/oct/bin source literal sets the top bit (`0x80y` → `SByte(-128)`)
/// — the lexer's two's-complement narrowing happens in FCS before
/// serialisation, so `as_u64` would fail.
fn fcs_typed_signed<T: TryFrom<i64>>(v: &Value, case: &str) -> T
where
    <T as TryFrom<i64>>::Error: std::fmt::Debug,
{
    let n = fields(v)[0]
        .as_i64()
        .unwrap_or_else(|| panic!("SynConst.{case} field 0 must be an integer JSON number"));
    T::try_from(n)
        .unwrap_or_else(|e| panic!("SynConst.{case} value {n} doesn't fit target type: {e:?}"))
}

// ---- AdjacentTag helpers ---------------------------------------------------

fn case_name(v: &Value) -> &str {
    v.get("Case")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected AdjacentTag DU case, got {v}"))
}

fn fields(v: &Value) -> &Vec<Value> {
    v.get("Fields")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected AdjacentTag DU fields, got {v}"))
}

/// Read an FCS `.NET string` payload emitted as raw UTF-16 code units. Older
/// fcs-dump binaries used a JSON string here; accept that shape too so ordinary
/// scalar-only dumps remain readable, but the raw array is the lossless format.
fn fcs_utf16_units(v: &Value, context: &str) -> Vec<u16> {
    match v {
        Value::Array(units) => units
            .iter()
            .map(|u| {
                let n = u
                    .as_u64()
                    .unwrap_or_else(|| panic!("{context} unit must be an integer, got {u}"));
                u16::try_from(n)
                    .unwrap_or_else(|e| panic!("{context} unit {n} does not fit u16: {e:?}"))
            })
            .collect(),
        Value::String(s) => s.encode_utf16().collect(),
        other => panic!("{context} must be a UTF-16 unit array, got {other}"),
    }
}

fn fcs_source_identifier_value(
    constant: &str,
    value: &Value,
    range: &Value,
    context: &str,
) -> NormalisedSourceIdentifierValue {
    let expanded = value
        .as_str()
        .unwrap_or_else(|| panic!("{context} must be a JSON string, got {value}"));
    match constant {
        "__LINE__" => NormalisedSourceIdentifierValue::Line(expanded.to_string()),
        "__SOURCE_FILE__" => {
            let file = fcs_range_file(range, context);
            let expected = Path::new(&file)
                .file_name()
                .unwrap_or_else(|| panic!("{context} range file {file:?} has no file name"))
                .to_string_lossy();
            if expanded != expected {
                panic!(
                    "{context} for __SOURCE_FILE__ was {expanded:?}, expected {expected:?} \
                     from range file {file:?}",
                );
            }
            NormalisedSourceIdentifierValue::SourceFile
        }
        "__SOURCE_DIRECTORY__" => {
            let file = fcs_range_file(range, context);
            let expected = Path::new(&file)
                .parent()
                .unwrap_or_else(|| panic!("{context} range file {file:?} has no parent directory"))
                .to_string_lossy();
            if expanded != expected {
                panic!(
                    "{context} for __SOURCE_DIRECTORY__ was {expanded:?}, expected {expected:?} \
                     from range file {file:?}",
                );
            }
            NormalisedSourceIdentifierValue::SourceDirectory
        }
        other => panic!("{context} has unsupported source identifier {other:?}"),
    }
}

fn fcs_range_file(range: &Value, context: &str) -> String {
    range
        .get("File")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{context} range must contain a JSON string File, got {range}"))
        .to_string()
}

/// A signed `int` field (a JSON number) — e.g. the `fieldNum` of a
/// `LibraryOnlyUnionCaseFieldGet`/`Set`, which FCS stores as a two's-complement
/// `int` (a high-bit literal is negative).
fn fcs_field_num(v: &Value) -> i32 {
    i32::try_from(
        v.as_i64()
            .unwrap_or_else(|| panic!("expected an integer field number, got {v}")),
    )
    .expect("field number fits in i32")
}

/// Read the source spelling out of an `IdentTrivia option` JSON value,
/// returning `None` for `None` / a `Some` whose case carries no source
/// spelling (`HasParenthesis`) / a mismatched JSON shape. Two cases carry the
/// original notation FCS rewrote away:
///
/// * `OriginalNotation` — the bare-operator form, used at infix/prefix
///   positions (`a + b`, `- x`). AdjacentTag encoding:
///   `{"Case": "Some", "Fields": [{"Case": "OriginalNotation",
///   "Fields": ["+"]}]}`; the spelling is field 0.
/// * `OriginalNotationWithParen` — the parenthesised operator-value /
///   operator-definition form (`(+)`, `Checked.(-)`), whose fields are
///   `(leftParenRange, text, rightParenRange)`, so the spelling is field **1**.
///
/// Both round-trip to the source operator (`+`, `-`), matching our green tree,
/// which stores the raw operator token under `IDENT_TOK` (without the parens —
/// those are sibling `LPAREN_TOK`/`RPAREN_TOK` the segment projection skips).
fn ident_original_notation(trivia_elem: &Value) -> Option<String> {
    if trivia_elem.get("Case").and_then(Value::as_str)? != "Some" {
        return None;
    }
    let outer = trivia_elem.get("Fields").and_then(Value::as_array)?;
    let inner = outer.first()?;
    let inner_fields = inner.get("Fields").and_then(Value::as_array)?;
    match inner.get("Case").and_then(Value::as_str)? {
        "OriginalNotation" => inner_fields.first()?.as_str().map(str::to_string),
        "OriginalNotationWithParen" => inner_fields.get(1)?.as_str().map(str::to_string),
        _ => None,
    }
}
