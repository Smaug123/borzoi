//! Slice 7a: the pure `definition_source` core against the real, shipped
//! `FSharp.Core.dll`.
//!
//! This is the offline half of go-to-definition into a referenced assembly: from
//! a method's `MethodDef` token + its DLL bytes, compute *where its source is* —
//! either embedded in the PDB or behind a SourceLink URL — with **no network**.
//! The handler shell (slice 7b) acts on the result; here we pin that the core
//! emits the right description, including the fetch *intent* (a `Remote` URL) for
//! a SourceLink-only definition like `printfn`.
//!
//! Requires the .NET 10 SDK on PATH (to build `tools/fcs-dump` once, which drops
//! the `FSharp.Core.dll` this reads); the Nix devShell provides it.

use borzoi::goto_source::{
    DefinitionSource, definition_document_for_range, definition_document_in_pdb, definition_source,
    definition_source_in_pdb, definition_source_with_range_fallback,
    entity_definition_document_in_pdb, entity_definition_source, entity_definition_source_in_pdb,
    entity_definition_source_with_range_fallback, range_definition_source_in_pdb,
};
use borzoi_assembly::pdb::{PortablePdb, embedded_portable_pdb};
use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, FsharpSourceRange, Member, MethodLike};

use crate::common::ensure_fsharp_core_dll;

/// The `metadata_token` of `Microsoft.FSharp.Core.<entity>`'s method whose F#
/// `source_name` is `source`.
fn method_token(bytes: &[u8], entity: &str, source: &str) -> u32 {
    let view = Ecma335Assembly::parse(bytes).expect("parse FSharp.Core");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");
    let e = entities
        .iter()
        .find(|e| {
            e.name == entity
                && e.namespace
                    .iter()
                    .map(String::as_str)
                    .eq(["Microsoft", "FSharp", "Core"])
        })
        .unwrap_or_else(|| panic!("entity Microsoft.FSharp.Core.{entity} not found"));
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.source_name.as_deref() == Some(source) => Some(m.metadata_token),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no method with source_name {source:?} on {entity}"))
}

#[test]
fn printfn_resolves_to_a_remote_sourcelink_url() {
    // `printfn` is SourceLink-only (not embedded), so the core emits a `Remote`
    // describing the GitHub fetch — the effect the handler will perform/surface.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let token = method_token(&bytes, "ExtraTopLevelOperators", "printfn");

    match definition_source(&bytes, token)
        .expect("definition source resolves")
        .expect("printfn has a definition source")
    {
        DefinitionSource::Remote {
            document,
            url,
            line,
            column,
        } => {
            assert!(
                document.ends_with("fslib-extra-pervasives.fs"),
                "printfn is defined in fslib-extra-pervasives.fs; got {document}"
            );
            // The SourceLink-mapped URL points at the document on GitHub, with
            // forward slashes and the matching file name.
            assert!(
                url.starts_with("https://raw.githubusercontent.com/"),
                "expected a GitHub raw URL; got {url}"
            );
            assert!(
                url.ends_with("fslib-extra-pervasives.fs") && !url.contains('\\'),
                "URL should name the source file with forward slashes; got {url}"
            );
            assert!(
                line >= 1 && column >= 1,
                "1-based position; got {line}:{column}"
            );
        }
        other => panic!("expected a Remote (SourceLink) source for printfn, got {other:?}"),
    }
}

#[test]
fn an_embedded_document_method_resolves_to_embedded_source() {
    // FSharp.Core embeds source for its generated files; a method defined in one
    // resolves to `Embedded` text — the offline path, no URL.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));

    // Find, via the PDB directly, the first method whose document embeds source.
    let image = embedded_portable_pdb(&bytes)
        .expect("debug directory parses")
        .expect("FSharp.Core ships an embedded portable PDB");
    let pdb = PortablePdb::read(&image).expect("parse the portable-PDB metadata image");
    let rid = (1..=pdb.method_debug_info_count())
        .find(|&rid| match pdb.method_first_sequence_point(rid) {
            Ok(Some(sp)) => matches!(pdb.document_embedded_source(sp.document), Ok(Some(_))),
            _ => false,
        })
        .expect("some FSharp.Core method is defined in an embedded document");
    let token = 0x0600_0000 | rid;

    match definition_source(&bytes, token)
        .expect("definition source resolves")
        .expect("the method has a definition source")
    {
        DefinitionSource::Embedded { text, line, .. } => {
            assert!(!text.is_empty(), "embedded source should be non-empty");
            assert!(line >= 1, "1-based line; got {line}");
        }
        other => panic!("expected Embedded source, got {other:?}"),
    }
}

#[test]
fn a_non_methoddef_token_has_no_source() {
    // A token that isn't a MethodDef (here a TypeDef token) maps to nothing.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let typedef_token = 0x0200_0001; // table 0x02 (TypeDef), row 1
    assert_eq!(definition_source(&bytes, typedef_token).unwrap(), None);
}

#[test]
fn a_non_methoddef_token_is_a_no_op_even_for_invalid_bytes() {
    // The token tag is checked before the PE is read, so a non-MethodDef token
    // is `Ok(None)` regardless of the bytes — never an `Err` (D5 contract).
    let typedef_token = 0x0200_0001;
    assert_eq!(definition_source(b"", typedef_token).unwrap(), None);
    assert_eq!(definition_source(b"not a PE", typedef_token).unwrap(), None);
}

// --- entity navigation (go-to-definition on a *type* or *module*) -----------

/// `(document, line)` of a [`DefinitionSource`], abstracting over the two
/// variants so a test can assert on the location regardless of how the source
/// is delivered.
fn doc_and_line(s: &DefinitionSource) -> (&str, u32) {
    match s {
        DefinitionSource::Embedded { document, line, .. }
        | DefinitionSource::Remote { document, line, .. } => (document, *line),
    }
}

/// The projected-member method tokens of `e` — the *resolution* view, which
/// (for unions/records and the like) deliberately drops source-mapped accessor
/// methods that [`Entity::method_def_tokens`] still carries.
fn surfaced_method_tokens(e: &Entity) -> Vec<u32> {
    e.members
        .iter()
        .filter_map(|m| match m {
            Member::Method(m) if m.metadata_token != 0 => Some(m.metadata_token),
            _ => None,
        })
        .collect()
}

/// `enumerate_type_defs` returns the top-level entities with their nested types
/// inside; flatten the whole forest so a test can sweep every entity.
fn flatten(entities: &[Entity]) -> Vec<&Entity> {
    fn go<'a>(e: &'a Entity, out: &mut Vec<&'a Entity>) {
        out.push(e);
        for n in &e.nested_types {
            go(n, out);
        }
    }
    let mut out = Vec::new();
    for e in entities {
        go(e, &mut out);
    }
    out
}

#[test]
fn a_module_entity_resolves_to_its_source() {
    // Navigating *the module* `ExtraTopLevelOperators` (not a specific member of
    // it) lands at its first source-mapped method — the top of the module in
    // source. `printfn` lives here, so we know the module has SourceLink source.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");
    let module = entities
        .iter()
        .find(|e| {
            e.name == "ExtraTopLevelOperators"
                && e.namespace
                    .iter()
                    .map(String::as_str)
                    .eq(["Microsoft", "FSharp", "Core"])
        })
        .expect("ExtraTopLevelOperators entity");

    let source = entity_definition_source(&bytes, &module.method_def_tokens)
        .expect("entity definition source resolves")
        .expect("the module has a definition source");
    let (document, line) = doc_and_line(&source);
    assert!(
        document.ends_with("fslib-extra-pervasives.fs"),
        "ExtraTopLevelOperators is defined in fslib-extra-pervasives.fs; got {document}"
    );
    assert!(line >= 1, "1-based line; got {line}");
}

#[test]
fn method_def_tokens_recover_navigation_that_members_alone_miss() {
    // Some F# entities (unions, records) hide their only source-mapped methods —
    // e.g. an instance property getter — from the resolution-oriented `members`.
    // Navigating such a type via the *projected* member tokens finds nothing,
    // but its *physical* `method_def_tokens` still reach the getter's sequence
    // point. Prove at least one such entity exists in FSharp.Core (the same shape
    // as FsCheck's `NonNull` union), so the recovery is exercised portably.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");

    let recovered = flatten(&entities).into_iter().find(|e| {
        let members_only = surfaced_method_tokens(e);
        entity_definition_source(&bytes, &members_only)
            .ok()
            .flatten()
            .is_none()
            && entity_definition_source(&bytes, &e.method_def_tokens)
                .ok()
                .flatten()
                .is_some()
    });
    assert!(
        recovered.is_some(),
        "expected an entity navigable only via its physical method_def_tokens"
    );
}

#[test]
fn entity_with_no_method_tokens_has_no_source() {
    // No candidate methods → nothing to navigate to (D5: say nothing).
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    assert_eq!(entity_definition_source(&bytes, &[]).unwrap(), None);
    // A non-MethodDef token among the candidates is ignored, not mistaken for a
    // method — still nothing.
    let typedef_token = 0x0200_0001;
    assert_eq!(
        entity_definition_source(&bytes, &[typedef_token]).unwrap(),
        None
    );
}

// --- definition *document* (where it is, even when source can't be opened) ---

#[test]
fn definition_document_reports_the_methods_origin() {
    // `printfn` carries sequence points (its document + line), even though its
    // source is SourceLink-only — the "say where it is" path hover uses.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let token = method_token(&bytes, "ExtraTopLevelOperators", "printfn");

    let doc = definition_document_in_pdb(&image, token)
        .expect("document resolves")
        .expect("printfn has a sequence point");
    assert!(
        doc.document.ends_with("fslib-extra-pervasives.fs"),
        "printfn is defined in fslib-extra-pervasives.fs; got {}",
        doc.document
    );
    assert!(doc.line >= 1 && doc.column >= 1);

    // The document/position agree with what the source-resolving core reports
    // (which additionally tries to obtain the text) — same point, two views.
    let source = definition_source_in_pdb(&image, token).unwrap().unwrap();
    let (src_doc, src_line) = doc_and_line(&source);
    assert_eq!((doc.document.as_str(), doc.line), (src_doc, src_line));
}

#[test]
fn entity_definition_document_reports_the_module_origin() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");
    let module = entities
        .iter()
        .find(|e| {
            e.name == "ExtraTopLevelOperators"
                && e.namespace
                    .iter()
                    .map(String::as_str)
                    .eq(["Microsoft", "FSharp", "Core"])
        })
        .expect("ExtraTopLevelOperators entity");

    let doc = entity_definition_document_in_pdb(&image, &module.method_def_tokens)
        .expect("document resolves")
        .expect("the module has a source-mapped method");
    assert!(
        doc.document.ends_with("fslib-extra-pervasives.fs"),
        "got {}",
        doc.document
    );
    assert!(doc.line >= 1);
}

// --- module values: the pickled-range fallback -------------------------------
//
// An F# module *value* (`let nan = …`) compiles to a static property whose
// getter merely reads the backing field: the getter MethodDef carries **no**
// sequence point (empirically: all 747 of FSharp.Core's module values), so the
// token path finds nothing. The signature pickle's `DefinitionRange` is the
// authoritative source position — the same one FCS/VS navigates to — and the
// PDB still says how to obtain the file (embedded source or SourceLink).

/// The `Microsoft.FSharp.Core.<entity>` method whose F# `source_name` is
/// `source` — the whole [`MethodLike`], where [`method_token`] returns just
/// the token.
fn fsharp_core_method(bytes: &[u8], entity: &str, source: &str) -> MethodLike {
    let view = Ecma335Assembly::parse(bytes).expect("parse FSharp.Core");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");
    let e = entities
        .iter()
        .find(|e| {
            e.name == entity
                && e.namespace
                    .iter()
                    .map(String::as_str)
                    .eq(["Microsoft", "FSharp", "Core"])
        })
        .unwrap_or_else(|| panic!("entity Microsoft.FSharp.Core.{entity} not found"));
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.source_name.as_deref() == Some(source) => Some(m.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no method with source_name {source:?} on {entity}"))
}

#[test]
fn a_module_value_resolves_via_its_pickled_definition_range() {
    // `nan` (IL `Operators.NaN`) is a plain module value.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let m = fsharp_core_method(&bytes, "Operators", "nan");
    assert!(m.module_value.is_some(), "nan is a value binding");

    // Precondition — the bug this fallback exists for: the value's getter
    // carries no sequence point, so the token path yields nothing.
    assert_eq!(
        definition_source_in_pdb(&image, m.metadata_token).unwrap(),
        None,
        "a module value's getter has no sequence point"
    );

    // The pickled DefinitionRange points at the *implementation* file (the
    // `.fs`, not the `.fsi` the primary val_range names for an
    // `.fsi`-constrained assembly like FSharp.Core).
    let range = m
        .definition_range
        .expect("a pickled module value carries its definition range");
    assert!(
        range.file.ends_with("prim-types.fs"),
        "nan is implemented in prim-types.fs; got {}",
        range.file
    );

    // The range resolves through SourceLink to the file at the pickled
    // position (converted to the 1-based DefinitionSource convention).
    match range_definition_source_in_pdb(&image, &range)
        .expect("range resolution succeeds")
        .expect("prim-types.fs is SourceLink-mapped")
    {
        DefinitionSource::Remote {
            document,
            url,
            line,
            column,
        } => {
            assert_eq!(document, range.file);
            assert!(
                url.starts_with("https://raw.githubusercontent.com/")
                    && url.ends_with("prim-types.fs")
                    && !url.contains('\\'),
                "SourceLink URL for prim-types.fs; got {url}"
            );
            assert_eq!(line, range.start_line, "1-based line passes through");
            assert_eq!(
                column,
                range.start_column + 1,
                "0-based pickled column becomes 1-based"
            );
        }
        other => panic!("expected Remote source for prim-types.fs, got {other:?}"),
    }
}

#[test]
fn the_range_fallback_composes_with_the_token_path() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();

    // A method *with* sequence points (printfn): the token wins; a decoy range
    // must not perturb the result.
    let printfn_token = method_token(&bytes, "ExtraTopLevelOperators", "printfn");
    let decoy = FsharpSourceRange {
        file: "Z:\\nowhere\\decoy.fs".into(),
        start_line: 1,
        start_column: 0,
        end_line: 1,
        end_column: 5,
    };
    assert_eq!(
        definition_source_with_range_fallback(&image, printfn_token, Some(&decoy)).unwrap(),
        definition_source_in_pdb(&image, printfn_token).unwrap(),
        "a sequence-pointed method ignores the fallback range"
    );

    // A module value (no sequence point): the fallback range resolves it.
    let nan = fsharp_core_method(&bytes, "Operators", "nan");
    let range = nan.definition_range.expect("nan carries a range");
    assert_eq!(
        definition_source_with_range_fallback(&image, nan.metadata_token, Some(&range)).unwrap(),
        range_definition_source_in_pdb(&image, &range).unwrap(),
        "a sequence-point-less method resolves via the range"
    );
    // …and with no range to fall back on, it stays `None` (D5).
    assert_eq!(
        definition_source_with_range_fallback(&image, nan.metadata_token, None).unwrap(),
        None
    );
}

#[test]
fn a_range_in_an_embedded_document_resolves_to_embedded_source() {
    // A range whose file *is* one of the PDB's embedded-source documents
    // resolves offline, exactly like the token path would.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let pdb = PortablePdb::read(&image).expect("parse the PDB image");
    let embedded_doc = (1..=pdb.document_count())
        .find(|&rid| matches!(pdb.document_embedded_source(rid), Ok(Some(_))))
        .expect("FSharp.Core embeds at least one document's source");
    let range = FsharpSourceRange {
        file: pdb.document_name(embedded_doc).unwrap(),
        start_line: 3,
        start_column: 4,
        end_line: 3,
        end_column: 9,
    };
    match range_definition_source_in_pdb(&image, &range)
        .unwrap()
        .unwrap()
    {
        DefinitionSource::Embedded {
            document,
            text,
            line,
            column,
        } => {
            assert_eq!(document, range.file);
            assert!(!text.is_empty());
            assert_eq!((line, column), (3, 5), "1-based position from the range");
        }
        other => panic!("expected Embedded source, got {other:?}"),
    }
}

#[test]
fn a_range_in_an_unknown_document_is_none() {
    // Neither a PDB document nor SourceLink-mapped → say nothing (D5).
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let range = FsharpSourceRange {
        file: "Q:\\not\\a\\real\\path.fs".into(),
        start_line: 1,
        start_column: 0,
        end_line: 1,
        end_column: 1,
    };
    assert_eq!(
        range_definition_source_in_pdb(&image, &range).unwrap(),
        None
    );
}

#[test]
fn definition_document_for_range_reports_the_pickled_position() {
    // The hover-side "say where it is" view built straight from the range —
    // no PDB needed: the range already names the document and position.
    let range = FsharpSourceRange {
        file: r"D:\build\prim-types.fs".into(),
        start_line: 4989,
        start_column: 12,
        end_line: 4989,
        end_column: 15,
    };
    let doc = definition_document_for_range(&range);
    assert_eq!(doc.document, range.file);
    assert_eq!(doc.line, 4989, "1-based line passes through");
    assert_eq!(doc.column, 13, "0-based pickled column becomes 1-based");
}

#[test]
fn a_pathological_pickled_column_saturates_rather_than_overflowing() {
    // The pickle is untrusted data: a malformed `u32::MAX` column must not
    // panic (debug) or wrap to column 0 (release) at the 0→1-based
    // conversion — it saturates, and the far-end clamp is harmless (the
    // editor-side conversion saturates back down).
    let range = FsharpSourceRange {
        file: "X.fs".into(),
        start_line: 1,
        start_column: u32::MAX,
        end_line: 1,
        end_column: u32::MAX,
    };
    assert_eq!(definition_document_for_range(&range).column, u32::MAX);
}

#[test]
fn definition_document_for_a_non_method_token_is_none() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let typedef_token = 0x0200_0001; // table 0x02 (TypeDef), not a MethodDef
    assert_eq!(
        definition_document_in_pdb(&image, typedef_token).unwrap(),
        None
    );
}

// --- entities: the pickled-range fallback ------------------------------------
//
// The entity counterpart of the module-value range fallback above. A method-
// less entity (a measure) or a value-only module carries no navigable sequence
// point, so its pickled `entity_range` is go-to-definition's only source — and
// for an `.fsi`-constrained assembly like FSharp.Core that range names the
// `.fsi`, which the PDB Document table never lists yet SourceLink still maps.

/// The `Microsoft.FSharp.…` entity at `namespace` with IL `name`.
fn fsharp_core_entity(bytes: &[u8], namespace: &[&str], name: &str) -> Entity {
    let view = Ecma335Assembly::parse(bytes).expect("parse FSharp.Core");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");
    flatten(&entities)
        .into_iter()
        .find(|e| {
            e.name == name
                && e.namespace
                    .iter()
                    .map(String::as_str)
                    .eq(namespace.iter().copied())
        })
        .unwrap_or_else(|| panic!("entity {}.{name} not found", namespace.join(".")))
        .clone()
}

#[test]
fn a_sequence_pointed_entity_ignores_the_fallback_range() {
    // `ListModule` has source-mapped methods, so the token sweep wins; a decoy
    // range must not preempt it (the entity analogue of the member-side check).
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let module = fsharp_core_entity(
        &bytes,
        &["Microsoft", "FSharp", "Collections"],
        "ListModule",
    );

    let decoy = FsharpSourceRange {
        file: "Z:\\nowhere\\decoy.fsi".into(),
        start_line: 1,
        start_column: 0,
        end_line: 1,
        end_column: 5,
    };
    let via_token = entity_definition_source_in_pdb(&image, &module.method_def_tokens).unwrap();
    assert!(via_token.is_some(), "ListModule has a source-mapped method");
    assert_eq!(
        entity_definition_source_with_range_fallback(
            &image,
            &module.method_def_tokens,
            Some(&decoy),
        )
        .unwrap(),
        via_token,
        "a sequence-pointed entity ignores the fallback range"
    );
}

#[test]
fn a_measure_entity_resolves_via_its_pickled_range() {
    // `Microsoft.FSharp.Data.UnitSystems.SI.UnitNames.metre` is a standalone
    // measure: an ECMA TypeDef row with zero methods, so the token sweep finds
    // nothing and only the pickled `entity_range` can navigate it.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let metre = fsharp_core_entity(
        &bytes,
        &[
            "Microsoft",
            "FSharp",
            "Data",
            "UnitSystems",
            "SI",
            "UnitNames",
        ],
        "metre",
    );
    assert!(
        metre.method_def_tokens.is_empty(),
        "a measure has no methods; got {:?}",
        metre.method_def_tokens
    );
    // Precondition: no token, so the token sweep yields nothing.
    assert_eq!(
        entity_definition_source_in_pdb(&image, &metre.method_def_tokens).unwrap(),
        None
    );

    let range = metre
        .definition_range
        .as_ref()
        .expect("a measure carries its entity range");
    assert!(
        range.file.ends_with("SI.fs"),
        "metre is defined in SI.fs; got {}",
        range.file
    );

    // The fallback resolves it, agreeing with resolving the range directly.
    let resolved =
        entity_definition_source_with_range_fallback(&image, &metre.method_def_tokens, Some(range))
            .expect("range resolution succeeds");
    assert_eq!(
        resolved,
        range_definition_source_in_pdb(&image, range).unwrap()
    );
    match resolved.expect("SI.fs is SourceLink-mapped") {
        DefinitionSource::Remote {
            document,
            url,
            line,
            column,
        } => {
            assert_eq!(document, range.file);
            assert!(
                url.starts_with("https://raw.githubusercontent.com/")
                    && url.ends_with("SI.fs")
                    && !url.contains('\\'),
                "SourceLink URL for SI.fs; got {url}"
            );
            assert_eq!(line, range.start_line, "1-based line passes through");
            assert_eq!(
                column,
                range.start_column + 1,
                "0-based column becomes 1-based"
            );
        }
        other => panic!("expected Remote source for SI.fs, got {other:?}"),
    }
}

#[test]
fn an_entity_range_naming_an_fsi_still_sourcelink_maps() {
    // `ListModule`'s pickled range names `list.fsi` — a signature file that
    // never appears in the PDB Document table (no sequence points), yet the
    // SourceLink prefix map still covers it. Probe finding 2, end-to-end.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let module = fsharp_core_entity(
        &bytes,
        &["Microsoft", "FSharp", "Collections"],
        "ListModule",
    );
    let range = module
        .definition_range
        .as_ref()
        .expect("ListModule carries an entity range");
    assert!(
        range.file.ends_with("list.fsi"),
        "ListModule's range names list.fsi; got {}",
        range.file
    );

    match range_definition_source_in_pdb(&image, range)
        .expect("range resolution succeeds")
        .expect("list.fsi is SourceLink-mapped despite no Document row")
    {
        DefinitionSource::Remote { document, url, .. } => {
            assert_eq!(document, range.file);
            assert!(
                url.starts_with("https://raw.githubusercontent.com/")
                    && url.ends_with("list.fsi")
                    && !url.contains('\\'),
                "SourceLink URL for list.fsi; got {url}"
            );
        }
        other => panic!("expected Remote source for list.fsi, got {other:?}"),
    }
}

#[test]
fn a_method_less_entity_reports_its_document_without_a_pdb() {
    // The hover "defined in" path: a measure has no method (nothing for
    // `entity_definition_document_in_pdb` to find) but its pickled range names
    // the document and position directly — no PDB needed. This is the
    // composition `entity_definition_document` performs.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let metre = fsharp_core_entity(
        &bytes,
        &[
            "Microsoft",
            "FSharp",
            "Data",
            "UnitSystems",
            "SI",
            "UnitNames",
        ],
        "metre",
    );
    // No token → the PDB half says nothing.
    assert_eq!(
        entity_definition_document_in_pdb(&image, &metre.method_def_tokens).unwrap(),
        None
    );
    // The range half needs no PDB at all.
    let range = metre
        .definition_range
        .as_ref()
        .expect("metre carries a range");
    let doc = definition_document_for_range(range);
    assert!(doc.document.ends_with("SI.fs"), "got {}", doc.document);
    assert_eq!(doc.line, range.start_line);
    assert_eq!(doc.column, range.start_column + 1);
}
