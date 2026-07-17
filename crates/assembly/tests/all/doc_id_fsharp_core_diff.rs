//! Differential for F# **module-value** documentation-comment IDs, against the
//! F# compiler's own `FSharp.Core.xml`.
//!
//! An F# module value (`let nan = …`) compiles to an IL *property* but is
//! surfaced as a method (FCS's source view); `project_fsharp_members` marks the
//! getter-rebranded value with `MethodLike::module_value` so `doc_id` keys it
//! `P:`, the prefix the F# compiler's own XML uses. This pins that end-to-end
//! against the real
//! `FSharp.Core`: **every `P:` key whose declaring type is a module must be
//! reproduced** by our generator.
//!
//! Scope: this is *not* a whole-file `xml ⊆ ours` check — FSharp.Core also has
//! large, unrelated `M:` (generic module methods / F# array-bound encoding),
//! `T:` (type naming), and dropped-type-property gaps tracked separately in
//! `docs/completed/fsharp-member-rebranding-docid-plan.md`. We assert only the module-value
//! subset this slice fixes, and *report* the residual gaps so a regression that
//! widens them is visible without coupling this test to that future work.
//!
//! Requires the .NET SDK on PATH (its `FSharp/` dir ships the dll+xml pair).

use std::collections::BTreeSet;

use borzoi_assembly::doc_id::walk_doc_ids;
use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, EntityKind};

use crate::common::ensure_sdk_fsharp_core;

/// The `<member name="…">` keys in a doc XML.
fn xml_keys(xml: &str) -> BTreeSet<String> {
    const OPEN: &str = "<member name=\"";
    let mut keys = BTreeSet::new();
    let mut rest = xml;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let end = after.find('"').expect("unterminated member name attribute");
        keys.insert(after[..end].to_string());
        rest = &after[end..];
    }
    keys
}

/// Fully-qualified names (`Namespace.Name`, arity-suffixed as stored) of every
/// module entity, recursively.
fn module_fqns(entities: &[Entity], out: &mut BTreeSet<String>) {
    for e in entities {
        if e.kind == EntityKind::Module {
            let mut fqn = String::new();
            if !e.namespace.is_empty() {
                fqn.push_str(&e.namespace.join("."));
                fqn.push('.');
            }
            fqn.push_str(&e.name);
            out.insert(fqn);
        }
        module_fqns(&e.nested_types, out);
    }
}

/// The declaring `Namespace.Type` of a `P:`/`M:`/`F:`/`E:` member key — the head
/// before the final `.member`, ignoring any `(args)`.
fn declaring(key: &str) -> Option<String> {
    let body = &key[2..];
    let head = &body[..body.find('(').unwrap_or(body.len())];
    head.rfind('.').map(|dot| head[..dot].to_string())
}

#[test]
fn module_value_property_keys_are_reproduced() {
    let (dll, xml_path) = ensure_sdk_fsharp_core();
    let bytes = std::fs::read(&dll).expect("read FSharp.Core.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    let types = view
        .enumerate_type_defs()
        .expect("enumerate FSharp.Core types");

    let mut ours = BTreeSet::new();
    for e in &types {
        walk_doc_ids(e, None, &mut |id| {
            ours.insert(id);
        });
    }

    let mut modules = BTreeSet::new();
    module_fqns(&types, &mut modules);

    let xml = std::fs::read_to_string(&xml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", xml_path.display()));
    let theirs = xml_keys(&xml);

    // The subset this slice owns: `P:` keys declared on a module — F# module
    // values, rebranded to methods but keyed `P:` by the F# compiler.
    let module_props: Vec<&String> = theirs
        .iter()
        .filter(|k| k.starts_with("P:"))
        .filter(|k| declaring(k).is_some_and(|d| modules.contains(&d)))
        .collect();

    // Sanity: FSharp.Core really does document a rich spread of module values, so
    // a mis-located dll/xml can't make this pass vacuously.
    assert!(
        module_props.len() >= 100,
        "expected a rich module-value sample from FSharp.Core; got {}",
        module_props.len(),
    );

    let missing: Vec<&String> = module_props
        .iter()
        .copied()
        .filter(|k| !ours.contains(*k))
        .collect();
    assert!(
        missing.is_empty(),
        "module-value `P:` keys not reproduced ({} of {}):\n{missing:#?}",
        missing.len(),
        module_props.len(),
    );
}
