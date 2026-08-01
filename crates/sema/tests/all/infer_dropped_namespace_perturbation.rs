//! A **perturbation oracle** for the per-namespace dropped-type marker
//! ([`AssemblyEnv::mark_namespace_dropped_type`]), over the whole inference
//! surface rather than one door at a time.
//!
//! The marker says: *some type in this namespace was dropped during projection,
//! so what survives there is not provably all of it.* The soundness consequence
//! is not a rule about any particular lookup — it is a statement about the whole
//! phase:
//!
//! > Under a marked namespace `N`, **nothing we publish may depend on the types
//! > that survived in `N`** — because the one FCS actually binds could be the one
//! > we lost.
//!
//! That is directly checkable by *perturbing the thing it claims irrelevance
//! from*. Build two envs from the same entity set, both with `N` marked: one
//! whole, one with every type in `N` **deleted**. If the phase honours the
//! marker, the whole env's published facts are a subset of the pruned env's, and
//! agree wherever both speak — the surviving occupants of `N` changed nothing,
//! because nothing was read from them. (Subset, not equality: deleting types can
//! *remove* an ambiguity, so the pruned side may commit where the whole side
//! defers. That direction is not a soundness failure.)
//!
//! Why this shape and not another door-by-door test: the doors an
//! assembly-supplied type enters inference through are not enumerable by the
//! compiler, and the one this oracle was written for
//! ([`borzoi_sema::infer`]'s literal-receiver member wake) was found by hand
//! after two others had been sealed. A per-door test pins the door you already
//! thought of; this pins the *property*, so a door nobody enumerated fails here
//! anyway — which is how it earned its keep on the first run, catching the base
//! chain's and the interface walk's own by-name reads.
//!
//! Its reach is nonetheless exactly [`SOURCES`] × [`perturbed_namespaces`]: the
//! property is universally quantified but the check is not, so a door no snippet
//! walks through is untested. Widening it means adding a snippet, and the
//! coverage floor below refuses one that publishes nothing.
//!
//! **And there is a whole failure mode it cannot see**, which is worth knowing
//! before trusting a green run. Perturbing by *deletion* proves the marked
//! namespace's survivors were never **read**. It says nothing about the phase
//! *concluding* something from their absence — because to this oracle "deleted"
//! and "present but unprovable" are the same input, so a decision keyed on the
//! difference reads identically on both sides. That is exactly the shape of the
//! bug `unproven_object_base_edge_sinks_the_chain_rather_than_capping` pins (an
//! unprovable `System.Object` edge graded as the universal-root cap): both sides
//! capped, both published the same, and this oracle passed. Catching that class
//! needs a reference that reads everything — FCS — not a self-perturbation.

use std::collections::HashMap;

use crate::common::{ensure_assembly_fixture_built, ensure_system_runtime_dll};
use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, infer_file, resolve_file,
};

/// Every source snippet the oracle runs, chosen to reach inference's distinct
/// assembly-type doors: a literal receiver (the path inference writes down
/// itself), an annotated receiver and a static callee (both keyed on an `Entity`
/// the resolver vetted), and receivers in the C# fixture's own namespaces.
const SOURCES: &[&str] = &[
    // Literal receiver — a data member, via a binder and directly.
    "module M\nlet s = \"hi\"\nlet n = s.Length\n",
    "module M\nlet n = \"hi\".Length\n",
    // Literal receiver — a single-candidate method call.
    "module M\nlet s = \"hi\"\nlet n = s.Substring(1)\n",
    // A member on the result of a member — the receiver path is a *metadata*
    // type name inference bridged, not one the resolver ever saw.
    "module M\nlet s = \"hi\"\nlet n = s.Substring(1).Length\n",
    // Annotated receiver (the `entity_annotation_ty` door).
    "module M\nlet s : System.String = \"hi\"\nlet n = s.Length\n",
    // Static callee (the `static_callee` door).
    "module M\nlet n = System.String.IsNullOrEmpty(\"hi\")\n",
    // An interface receiver: its member sources are the inherited-interface walk
    // and the `System.Object` cap, both reached by name from *another* namespace
    // than the receiver's own.
    "module M\nlet f (e : System.Collections.IEnumerable) = e.GetHashCode()\n",
    "module M\nlet f (l : System.Collections.IList) = l.Count\n",
    // The same, but with a receiver outside `System` — the only shape that keeps
    // the interface walk's `System.Object` cap reachable while `System` is marked.
    "module M\nlet f (x : Demo.IProbe) = x.GetHashCode()\n",
    // Fixture receivers, so a namespace other than `System` also bites.
    "module M\nlet f (w : Demo.Widget) = w.Count\n",
    "module M\nlet f (t : Demo.Sub.Deep) = t.ToString()\n",
    "module M\nlet f (t : Demo.Thing) = t.Value\n",
];

/// The namespaces perturbed. Each must contain at least one type in the entity
/// set (asserted below) — a namespace the pruning does not actually empty would
/// make its whole row vacuous.
fn perturbed_namespaces() -> Vec<Vec<String>> {
    vec![
        vec!["System".to_string()],
        vec!["System".to_string(), "Collections".to_string()],
        vec!["Demo".to_string()],
        vec!["Demo".to_string(), "Sub".to_string()],
        vec!["ExtColl".to_string()],
    ]
}

/// The BCL plus the C# fixture, as owned entities so a namespace can be pruned
/// out of the set before the env is built.
fn entities() -> Vec<Entity> {
    let bcl = std::fs::read(ensure_system_runtime_dll()).expect("read System.Runtime.dll");
    let fixture = std::fs::read(ensure_assembly_fixture_built()).expect("read fixture dll");
    let mut all = Ecma335Assembly::parse(&bcl)
        .expect("parse System.Runtime.dll")
        .enumerate_type_defs()
        .expect("enumerate System.Runtime types");
    all.extend(
        Ecma335Assembly::parse(&fixture)
            .expect("parse fixture dll")
            .enumerate_type_defs()
            .expect("enumerate fixture types"),
    );
    // An interface receiver whose *own* namespace survives a marked `System`. The
    // interface walk appends `System.Object` as a member source by name, and that
    // read is only reachable when the receiver's annotation still resolves — which
    // rules out every BCL interface (their `System.*` heads decline first) and the
    // C# fixture (it declares no interface at all). Cloned from a real non-generic
    // interface so only the discriminating fields are hand-written.
    let template = all
        .iter()
        .find(|e| e.namespace == ["System", "Collections"] && e.name == "IEnumerable")
        .expect("System.Collections.IEnumerable template")
        .clone();
    all.push(Entity {
        namespace: vec!["Demo".to_string()],
        name: "IProbe".to_string(),
        interfaces: Vec::new(),
        members: Vec::new(),
        method_def_tokens: Vec::new(),
        nested_types: Vec::new(),
        ..template
    });
    all
}

/// Everything one run publishes, keyed so the two sides are comparable: each
/// expression type by its source range, each binder type by the binder's name,
/// and each member resolution by its use range (rendered, since `Resolution`
/// holds handles whose numbering differs between two differently-sized envs).
fn published(env: &AssemblyEnv, src: &str) -> HashMap<String, String> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "parse errors in {src:?}");
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env);

    let mut out = HashMap::new();
    for (range, ty) in inferred.types() {
        out.insert(format!("expr@{range:?}"), ty.render());
    }
    for (def, ty) in inferred.def_types() {
        out.insert(format!("def:{}", resolved.def(*def).name), ty.render());
    }
    for (range, res) in inferred.member_resolutions() {
        // Rendered through the env: a `Resolution::Member` carries an
        // `EntityHandle` into *this* env's node arena, and the two envs number
        // their nodes differently (the pruned one holds fewer). Comparing the raw
        // handles would report a difference for every member the perturbation left
        // alone, so the identity is spelled out instead.
        let rendered = match res {
            Resolution::Member { parent, idx } => format!(
                "{}.{}",
                env.entity_full_name(*parent),
                env.member_display_name(*parent, *idx)
            ),
            other => format!("{other:?}"),
        };
        out.insert(format!("member@{range:?}"), rendered);
    }
    out
}

/// The oracle. For each perturbed namespace `N`: with `N` marked on both sides,
/// deleting `N`'s surviving types must not remove or change anything we publish.
#[test]
fn a_marked_namespace_contributes_nothing_we_publish() {
    let all = entities();
    // Coverage floor: a source that publishes nothing even unperturbed would make
    // its whole row vacuous, and the oracle would pass while measuring nothing.
    let control = AssemblyEnv::from_entities(all.clone());
    for src in SOURCES {
        assert!(
            !published(&control, src).is_empty(),
            "no facts published for {src:?} even with nothing marked — this source \
             measures nothing, so fix it rather than leaving a vacuous row"
        );
    }

    for namespace in perturbed_namespaces() {
        let pruned_entities: Vec<Entity> = all
            .iter()
            .filter(|e| e.namespace != namespace)
            .cloned()
            .collect();
        assert!(
            pruned_entities.len() < all.len(),
            "namespace {namespace:?} holds no type in the entity set, so pruning it \
             perturbs nothing"
        );

        let mut whole = AssemblyEnv::from_entities(all.clone());
        whole.mark_namespace_dropped_type(namespace.clone());
        let mut pruned = AssemblyEnv::from_entities(pruned_entities);
        pruned.mark_namespace_dropped_type(namespace.clone());

        let mut checked = 0usize;
        for src in SOURCES {
            let whole_facts = published(&whole, src);
            let pruned_facts = published(&pruned, src);
            checked += whole_facts.len();
            for (key, value) in &whole_facts {
                assert_eq!(
                    pruned_facts.get(key),
                    Some(value),
                    "with {namespace:?} marked dropped, {src:?} published {key} = {value} \
                     from a type in {namespace:?} — a marked namespace's survivors must \
                     contribute nothing, since the one FCS binds may be the one we lost"
                );
            }
        }
        // The second floor, and the one that would notice a *regression into
        // silence*: a row where the marked side publishes nothing compares nothing,
        // and passes. The counts today are 16 / 44 / 37 / 48 / 51 in the order
        // above — `System` is the low one because nearly every base edge ends
        // there, so marking it sinks nearly every chain and the member results go
        // with them. What survives is what owes the assemblies nothing: a literal's
        // own type, and an annotation head outside the marked namespace.
        assert!(
            checked > 0,
            "marking {namespace:?} deferred every fact in every source, so this row \
             compared nothing — the oracle needs a source it does not silence"
        );
    }
}
