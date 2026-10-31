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
//! `/`-separated chain of metadata names, each with its `` `n `` arity
//! suffix; the definition is the type whose `TypeDef` was written with
//! exactly that name and suffix (not the parameter count, which the suffix
//! need not match, and never an F# pickle marker, which has no `TypeDef`) in
//! exactly that assembly, following the assembly's type forwarders when it
//! only forwards the name. Accessibility plays no part — Roslyn imports every
//! type of a referenced assembly.
//!
//! The assembly is bound by identity, as Roslyn's reference manager binds an
//! `AssemblyRef`: the sole loaded DLL with the reference's simple name, and
//! only when that DLL's public-key token is the reference's — a reference
//! across tokens (or between a strong and a weak name) stays unresolved.
//! Version does not take part: Roslyn unifies a reference with the one
//! definition of its name whether that definition's version is higher or
//! lower. A forwarder's `AssemblyRef` is bound the same way.
//!
//! Exact or nothing: an assembly name that no loaded DLL — or more than one —
//! carries, a name that matches twice, a name some type holds with no suffix
//! recorded (a marker, or a suffix the projection does not take as an arity,
//! which a same-module reference spells stripped), or a namespace in which
//! some DLL dropped an undecodable type, all decline with a typed reason.

use borzoi_assembly::{AssemblyIdentity, TypeRef};

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
    /// Two loaded DLLs carry the referenced assembly's simple name (or one
    /// carries it in another case, which a binder may match, or some
    /// manifest qualifies the name by culture or flags, which a binder also
    /// matches), or two types match, or a type with no suffix recorded may be
    /// the one named: which one a consumer binds is not knowable here.
    Ambiguous,
    /// A DLL dropped an undecodable type in the reference's namespace, so the
    /// type we would bind may not be the only candidate.
    DroppedNearby,
    /// A forwarder chain longer than any real facade layering.
    ForwarderChainTooLong,
    /// The one loaded DLL with the referenced assembly's simple name carries
    /// another public-key token, or its identity was not read. Roslyn leaves
    /// a reference across tokens unresolved — unless retargeting maps it
    /// elsewhere, which is not modelled — so this binds nowhere certain.
    IdentityMismatch,
}

/// More forwarder hops than any real facade layering has (`netstandard` →
/// `System.Runtime` → `System.Private.CoreLib` is two).
const FORWARDER_FUEL: usize = 8;

/// One segment of a reference's name: the name without its `` `n `` suffix
/// (as the projection keeps both type and reference names), and that suffix
/// ([`borzoi_assembly::Entity::metadata_arity_suffix`] on the definition).
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
            Some(identity) => match self.assembly_key_for(identity) {
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
            key = match self.assembly_key_for(redirect) {
                Ok(key) => key,
                Err(why) => return why,
            };
        }
        IlTypeDefinition::ForwarderChainTooLong
    }

    /// The key of the loaded DLL `reference` binds to: the sole one with its
    /// simple name, and only when that one's public-key token is the
    /// reference's. Simple names compare case-insensitively (Roslyn's
    /// `AssemblyIdentityComparer`, the CLR binder alike), so a loaded DLL
    /// whose name differs from the reference's only in case could be the one
    /// bound: the binding is ambiguous. A binder also matches culture and
    /// flags (content type, `Retargetable`), which a `TypeRef` does not carry,
    /// so a name some manifest qualifies by either — or any name, when those
    /// were not read — is ambiguous too.
    fn assembly_key_for(
        &self,
        reference: &AssemblyIdentity,
    ) -> Result<AssemblyKey<'_>, IlTypeDefinition> {
        let key = self.assembly_key_named(&reference.name)?;
        let bound = match key {
            AssemblyKey::Provenance(id) => self
                .assembly_identities
                .get(id.0 as usize)
                .and_then(Option::as_ref),
            AssemblyKey::Identity(identity) => Some(identity),
        };
        match bound {
            Some(definition) if definition.public_key_token == reference.public_key_token => {
                Ok(key)
            }
            _ => Err(IlTypeDefinition::IdentityMismatch),
        }
    }

    /// The key of the sole loaded DLL with simple name `name` — see
    /// [`Self::assembly_key_for`], which also checks the token.
    fn assembly_key_named(&self, name: &str) -> Result<AssemblyKey<'_>, IlTypeDefinition> {
        if !self.qualified_names_read || self.qualified_names.contains(&name.to_lowercase()) {
            return Err(IlTypeDefinition::Ambiguous);
        }
        let differs_in_case = |other: &str| {
            other != name
                && (other.to_lowercase() == name.to_lowercase()
                    || other.to_uppercase() == name.to_uppercase())
        };
        if self
            .assembly_identities
            .iter()
            .flatten()
            .any(|a| differs_in_case(&a.name))
            || self
                .top_level_types
                .iter()
                .any(|&h| differs_in_case(&self.entity(h).assembly.name))
        {
            return Err(IlTypeDefinition::Ambiguous);
        }
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
        let named = types
            .all
            .iter()
            .copied()
            .filter(|&h| self.assembly_key(h) == key && self.entity(h).name == bare);
        bind_segment(self, named, arity)
    }

    /// Descend from `parent` through the nested segments `rest`.
    fn descend(&self, parent: EntityHandle, rest: &[Segment<'_>]) -> IlTypeDefinition {
        let Some((&(bare, arity), rest)) = rest.split_first() else {
            return IlTypeDefinition::Resolved(parent);
        };
        let named = self
            .children(parent)
            .iter()
            .copied()
            .filter(|&c| self.entity(c).name == bare);
        match bind_segment(self, named, arity) {
            Ok(Some(child)) => self.descend(child, rest),
            Ok(None) => IlTypeDefinition::NotFound,
            Err(why) => why,
        }
    }
}

/// The one of `named` (the candidates with a segment's bare name) whose
/// `TypeDef` was written with suffix `arity`. A candidate with no suffix
/// recorded — a pickle marker, or a `TypeDef` whose suffix the projection
/// does not take as an arity (`` Foo`0 ``), which a same-module reference
/// spells stripped, exactly like a reference to a genuine `Foo` — may be the
/// one the reference names, so any such candidate makes the segment
/// ambiguous.
fn bind_segment(
    env: &AssemblyEnv,
    named: impl Iterator<Item = EntityHandle>,
    arity: usize,
) -> Result<Option<EntityHandle>, IlTypeDefinition> {
    let mut found = None;
    for h in named {
        match env.entity(h).metadata_arity_suffix {
            None => return Err(IlTypeDefinition::Ambiguous),
            Some(n) if n == arity => {
                if found.replace(h).is_some() {
                    return Err(IlTypeDefinition::Ambiguous);
                }
            }
            Some(_) => {}
        }
    }
    Ok(found)
}
