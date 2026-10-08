//! Binding an IL type reference to its definition the way a *metadata*
//! consumer binds it — Roslyn's PE importer, the CLR loader — rather than the
//! way F# name resolution does.
//!
//! The env's type-position lookups ([`AssemblyEnv::lookup_type`] and the
//! abbreviation chase) work in F#'s logical-name domain and keep F#'s rules:
//! public-only, source names, first-wins slots. A consumer reproducing a C#
//! tool's reading of metadata (the `<inheritdoc>` expansion in the LSP) needs
//! the other reading: a `TypeRef` names one assembly by simple name (or, with
//! `assembly: None`, the referencing type's own module), a namespace, and a
//! `/`-separated chain of metadata names, each with the type parameters it
//! introduces; the definition is the type of exactly that name and arity in
//! exactly that assembly, following the assembly's type forwarders when it
//! only forwards the name. Accessibility plays no part — Roslyn imports every
//! type of a referenced assembly.
//!
//! Exact or nothing: an assembly name that no loaded DLL — or more than one —
//! carries, a name that matches twice, or a namespace in which some DLL
//! dropped an undecodable type, all decline with a typed reason.

use borzoi_assembly::TypeRef;

use super::{AssemblyEnv, AssemblyKey, EntityHandle};

/// Where [`AssemblyEnv::il_type_definition`] lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IlTypeDefinition {
    /// The definition.
    Resolved(EntityHandle),
    /// Not a named type (a primitive, an array, a type variable, …).
    NotNamed,
    /// No loaded DLL carries the referenced assembly's simple name, the
    /// assembly carries no type of that name, or the reference's per-segment
    /// arities do not line up with its name.
    NotFound,
    /// Two loaded DLLs carry the referenced assembly's simple name, or two
    /// types match: which one a consumer binds is not knowable here.
    Ambiguous,
    /// A DLL dropped an undecodable type in the reference's namespace, so the
    /// type we would bind may not be the only candidate.
    DroppedNearby,
    /// A forwarder chain longer than any real facade layering.
    ForwarderChainTooLong,
}

/// More forwarder hops than any real facade layering has (`netstandard` →
/// `System.Runtime` → `System.Private.CoreLib` is two).
const FORWARDER_FUEL: usize = 8;

/// One segment of a reference's name: the name without its `` `n `` suffix
/// (as the projection keeps both type and reference names), and the number of
/// type parameters the segment introduces.
type Segment<'a> = (&'a str, usize);

impl AssemblyEnv {
    /// The definition the IL type reference `ty`, written in the metadata of
    /// the assembly `from` belongs to, binds to — see the module docs.
    pub fn il_type_definition(&self, from: EntityHandle, ty: &TypeRef) -> IlTypeDefinition {
        let TypeRef::Named {
            assembly,
            namespace,
            name,
            segment_arities,
            ..
        } = ty
        else {
            return IlTypeDefinition::NotNamed;
        };
        let names: Vec<&str> = name.split('/').collect();
        if segment_arities.len() != names.len() {
            return IlTypeDefinition::NotFound;
        }
        let segments: Vec<Segment<'_>> = names
            .into_iter()
            .zip(segment_arities.iter().copied())
            .collect();
        if self.namespace_has_dropped_type(namespace) {
            return IlTypeDefinition::DroppedNearby;
        }
        let mut key = match assembly {
            None => self.assembly_key(from),
            Some(identity) => match self.assembly_key_named(&identity.name) {
                Ok(key) => key,
                Err(why) => return why,
            },
        };
        for _ in 0..FORWARDER_FUEL {
            let top = match self.top_level_in(key, namespace, segments[0]) {
                Ok(top) => top,
                Err(why) => return why,
            };
            if let Some(top) = top {
                return self.descend(top, &segments[1..]);
            }
            // Not declared here: a forwarder of this assembly may say where
            // the type lives now. Forwarders are keyed by the raw metadata
            // name, suffix included.
            let AssemblyKey::Provenance(id) = key else {
                return IlTypeDefinition::NotFound;
            };
            let (bare, arity) = segments[0];
            let raw = if arity == 0 {
                bare.to_string()
            } else {
                format!("{bare}`{arity}")
            };
            let Some(redirect) =
                self.assembly_forwarders[id.0 as usize].get(&(namespace.join("."), raw))
            else {
                return IlTypeDefinition::NotFound;
            };
            key = match self.assembly_key_named(redirect) {
                Ok(key) => key,
                Err(why) => return why,
            };
        }
        IlTypeDefinition::ForwarderChainTooLong
    }

    /// The key of the sole loaded DLL with simple name `name`.
    fn assembly_key_named(&self, name: &str) -> Result<AssemblyKey<'_>, IlTypeDefinition> {
        if let Some(key) = self.unique_assembly_key_for_name(name) {
            return Ok(key);
        }
        let any = self
            .assembly_identities
            .iter()
            .flatten()
            .any(|a| a.name == name)
            || self
                .top_level_types
                .iter()
                .any(|&h| self.entity(h).assembly.name == name);
        Err(if any || self.assembly_identities_incomplete {
            IlTypeDefinition::Ambiguous
        } else {
            IlTypeDefinition::NotFound
        })
    }

    /// The top-level type of DLL `key` in `namespace` with the name and arity
    /// of `segment`, if it declares one.
    fn top_level_in(
        &self,
        key: AssemblyKey<'_>,
        namespace: &[String],
        (bare, arity): Segment<'_>,
    ) -> Result<Option<EntityHandle>, IlTypeDefinition> {
        let Some(types) = self.types_by_namespace.get(namespace) else {
            return Ok(None);
        };
        let mut found = types.all.iter().copied().filter(|&h| {
            let e = self.entity(h);
            self.assembly_key(h) == key && e.name == bare && e.generic_parameters.len() == arity
        });
        match (found.next(), found.next()) {
            (None, _) => Ok(None),
            (Some(h), None) => Ok(Some(h)),
            (Some(_), Some(_)) => Err(IlTypeDefinition::Ambiguous),
        }
    }

    /// Descend from `parent` through the nested segments `rest`. A nested
    /// type's metadata redeclares its enclosers' type parameters, so the
    /// segment's own arity is the difference.
    fn descend(&self, parent: EntityHandle, rest: &[Segment<'_>]) -> IlTypeDefinition {
        let Some((&(bare, arity), rest)) = rest.split_first() else {
            return IlTypeDefinition::Resolved(parent);
        };
        let parent_arity = self.entity(parent).generic_parameters.len();
        let mut found = self.children(parent).iter().copied().filter(|&c| {
            let e = self.entity(c);
            e.name == bare && e.generic_parameters.len().checked_sub(parent_arity) == Some(arity)
        });
        match (found.next(), found.next()) {
            (None, _) => IlTypeDefinition::NotFound,
            (Some(child), None) => self.descend(child, rest),
            (Some(_), Some(_)) => IlTypeDefinition::Ambiguous,
        }
    }
}
