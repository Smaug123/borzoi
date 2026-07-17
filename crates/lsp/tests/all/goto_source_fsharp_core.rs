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
    DefinitionSource, definition_document_in_pdb, definition_source, definition_source_in_pdb,
    entity_definition_document_in_pdb, entity_definition_source,
};
use borzoi_assembly::pdb::{PortablePdb, embedded_portable_pdb};
use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Member};

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
