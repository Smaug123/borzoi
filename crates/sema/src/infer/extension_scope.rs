//! The extension-absence gate an overloaded call consults ([`ExtensionScope`]).

use super::*;

/// The OV-6 overload engine's **extension-absence gate** (see
/// `docs/overload-resolution-plan.md` §4.1(4) and
/// `docs/extension-scope-enumeration-plan.md`). An overloaded call `recv.M(args)`
/// may commit an *intrinsic* winner only when no in-scope F#-style or C#-style
/// extension member **named `M`** could join FCS's method group (the P15 landmine:
/// an applicable extension can beat an applicable intrinsic on any betterness rule
/// before rule 10).
///
/// The gate keeps the OV-6 *source set* — which is complete by construction after
/// eight review rounds — but refines each source from a boolean ("any extension
/// source in scope ⇒ defer") to a **name test** ("an extension *named `M`* in
/// scope ⇒ defer"), because an in-scope extension competes only within its own
/// name's group (probed — the enumeration plan §1). The sources:
///
/// - **Assembly side — name-keyed (EX-1/EX-2).** The env's auto-open surfaces, the
///   file's in-scope namespace chain (the always-in-scope root `[]` plus each
///   declared namespace and its ancestors), and the file's explicit
///   `open <namespace>` targets ([`Self::in_scope_namespaces`], which folds both).
///   [`AssemblyEnv::extension_named_in_scope`] answers, keyed by the call shape
///   (`is_static` — a value receiver's group takes only *instance* extensions, a
///   type-qualified call's only *static* ones). An unenumerable surface there
///   (projection failure, a dropped type, a contested auto-open) still defers for
///   every name.
/// - **`open`s the resolver could not name-key (EX-2 ⇒ [`Self::opens_unknowable`]).**
///   A project module/namespace open (EX-3), an assembly-module or `open type`, or
///   an opaque / vetoed / dropped-path open — the resolver marks these and the gate
///   defers *every* call in the file, exactly as the pre-EX-2 presence gate did for
///   *any* `open`.
/// - **Project side — name-keyed where walkable ([`Self::augmentation_instance_names`] /
///   [`Self::augmentation_static_names`], EX-3 §2(a)/(b)), presence-based for the
///   rest ([`Self::project_source_present`]).** A `type … with` augmentation
///   defers exactly its member names (own-file and preceding Compile-order files
///   alike); an augmentation member the walk cannot name, or an attribute the
///   resolver cannot prove is no C#-style `[<Extension>]` (EX-3 §2(d)), still
///   defers wholesale — own-file or threaded from a preceding file
///   ([`ResolvedFile::preceding_declares_extension_source`]). An `[<AutoOpen>]`
///   module is no trigger of its own (AO-1): its extension-capable contents are
///   exactly those two signals, both collected file-globally.
///
/// A source whose names cannot be computed **stays a wholesale defer**, so every
/// refinement strictly *shrinks* the deferred set and can never license a commit a
/// full name enumeration would not. The [`Default`] scope (no source, no opened
/// namespace, nothing unknowable) is used by in-crate unit tests that drive [`Gen`]
/// without a file.
#[derive(Default)]
pub(super) struct ExtensionScope {
    /// A **project** extension source we could not name-key is in scope: an
    /// augmentation member with an un-walkable name (own-file or preceding —
    /// EX-3 §2(a)/(b) thread the walkable ones as the sets below), or an
    /// attribute the resolver could not prove is no C#-style `[<Extension>]`
    /// (EX-3 §2(d)). Presence-based and name-blind: every method-call commit
    /// defers when set, whatever the name.
    project_source_present: bool,
    /// Some explicit `open` in the file brings an extension surface the resolver
    /// could not name-key (EX-2): a project module/namespace, an assembly module /
    /// `open type`, or an opaque / vetoed / dropped-path open. Presence-based like
    /// [`Self::project_source_present`] — every commit defers when set — and set
    /// straight from [`ResolvedFile::open_extension_unknowable`].
    opens_unknowable: bool,
    /// The file's **in-scope namespace chain plus its explicit `open <namespace>`
    /// targets**: the always-in-scope root `[]`, each declared namespace and its
    /// ancestors, and every assembly namespace an `open` brought in (EX-2,
    /// [`ResolvedFile::open_extension_namespaces`]). A referenced-assembly extension
    /// declared in any of these is in scope — an enclosing namespace needs no
    /// `open`, an `open`ed one is opened — so the assembly-side gate is asked the
    /// same query about all of them per call.
    in_scope_namespaces: Vec<Vec<String>>,
    /// The same-file augmentation member names, keyed by call shape (EX-3
    /// §2(a)): a `type … with` member joins **its own name's** call group, so
    /// only calls of these names defer. Wholesale unknowability (an
    /// un-walkable member name) folds into
    /// [`Self::project_source_present`] instead. Both sets empty in
    /// [`Default`], matching the no-source unit-test scope.
    augmentation_instance_names: crate::hash::HashSet<String>,
    augmentation_static_names: crate::hash::HashSet<String>,
}

impl ExtensionScope {
    pub(super) fn of(resolved: &ResolvedFile, env: &AssemblyEnv) -> ExtensionScope {
        // The root must be included explicitly — `namespace_paths` omits it, and it
        // is always in scope (so `module M` sees a global `[<Extension>]`). The
        // file's explicit `open <namespace>` targets (EX-2) join it: opening a
        // namespace makes its extensions in-scope exactly as an enclosing namespace
        // does, so the same per-namespace query serves both.
        let in_scope_namespaces = std::iter::once(Vec::new())
            .chain(resolved.namespace_paths().iter().cloned())
            .chain(resolved.open_extension_namespaces().iter().cloned())
            .collect();
        let project_source_present =
            // A preceding Compile-order file's extension source the fold could
            // not name-key (its walkable augmentation names thread as the
            // `preceding_*` sets below instead).
            resolved.preceding_declares_extension_source()
            // This file's own declarations: an augmentation member whose NAME
            // could not be walked (EX-3 §2(a) — the name-keyed sets below
            // carry the walkable ones), or an attribute that **may declare an
            // extension** — EX-3 §2(d) stage 5: the old "any attribute"
            // trigger, refined through the resolver's per-attribute type
            // resolutions (differentially validated against FCS). An
            // attribute resolving to a concrete non-`ExtensionAttribute` type
            // provably marks nothing; one resolving TO the marker, or that
            // the resolver declined to pin, keeps the presence defer. An
            // `[<AutoOpen>]` module as such is NOT a trigger (AO-1): both its
            // extension-capable content kinds — augmentations and
            // `[<Extension>]` attributes — are collected file-globally by the
            // two walks above, nested modules included, so the old presence
            // defer was pure over-approximation. Explicit `open`s are not here
            // either — EX-2 routes each open's target to `in_scope_namespaces`
            // (name-keyed) or `opens_unknowable` (defer).
            || resolved.augmentation_names_unknowable()
            || resolved.attributes_may_declare_extension(env);
        // Own-file and preceding-file augmentation names key the same
        // per-name check (EX-3 §2(a)/(b)): a preceding same-namespace
        // augmentation is in scope with no `open`, exactly like an own-file
        // one, so the union is the file's project-side extension name set.
        let augmentation_instance_names = resolved
            .augmentation_instance_names()
            .union(resolved.preceding_augmentation_instance_names())
            .cloned()
            .collect();
        let augmentation_static_names = resolved
            .augmentation_static_names()
            .union(resolved.preceding_augmentation_static_names())
            .cloned()
            .collect();
        ExtensionScope {
            project_source_present,
            opens_unknowable: resolved.open_extension_unknowable(),
            in_scope_namespaces,
            augmentation_instance_names,
            augmentation_static_names,
        }
    }

    /// Whether an extension member **named `name`** is provably absent from the
    /// file's in-scope extension surface — the precondition for committing an
    /// intrinsic overload of that name (`is_static` selects the call shape, since
    /// FCS takes only *instance* extensions into a value receiver's group and only
    /// *static* ones into a type-qualified call's).
    ///
    /// **Name-keyed on the assembly side (EX-1), for explicit `open <namespace>`
    /// targets (EX-2), and for project augmentations (EX-3 §2(a)/(b));
    /// presence-based for what those walks could not name-key.** An in-scope
    /// extension competes only within its *own* name's group (probed —
    /// `docs/extension-scope-enumeration-plan.md` §1), so a surface that provably
    /// lacks `name` is no reason to defer; that conflation is what made every
    /// FSharp.Core-referencing project defer every overloaded call. The remaining
    /// name-blind triggers ([`Self::project_source_present`],
    /// [`Self::opens_unknowable`]) still defer wholesale — sound.
    pub(super) fn absent(&self, env: &AssemblyEnv, name: &str, is_static: bool) -> bool {
        !self.project_source_present
            && !self.opens_unknowable
            // EX-3 §2(a): a same-file augmentation member joins its own
            // name's group only — the name-keyed project-side sibling of the
            // assembly query below.
            && !if is_static {
                self.augmentation_static_names.contains(name)
            } else {
                self.augmentation_instance_names.contains(name)
            }
            && !env.extension_named_in_scope(&self.in_scope_namespaces, name, is_static)
    }
}

#[cfg(test)]
mod tests {
    use super::ExtensionScope;
    use crate::AssemblyEnv;

    #[test]
    fn extension_scope_absent_decision_logic() {
        // EX-1: the gate is **name-keyed on the assembly side** and presence-based on
        // the project side. With an empty env (no referenced assembly can contribute
        // anything) and no project source, every name is absent.
        let env = AssemblyEnv::default();
        let extension_free = ExtensionScope::default();
        assert!(
            extension_free.absent(&env, "Substring", false),
            "an extension-free scope over an empty env makes every name absent"
        );
        assert!(
            extension_free.absent(&env, "Compare", true),
            "…for a static call too"
        );

        // A **project** source (an augmentation, an attribute, an auto-open) is
        // still name-blind — the resolver does not yet export what it declares
        // (EX-3) — so it defers wholesale, whatever the name.
        let project_source = ExtensionScope {
            project_source_present: true,
            ..ExtensionScope::default()
        };
        assert!(
            !project_source.absent(&env, "Substring", false),
            "a project extension source defers every name (presence-based, EX-3)"
        );
        assert!(
            !project_source.absent(&env, "NoSourceDeclaresThis", false),
            "…including a name no source declares — that is the remaining coverage cost"
        );

        // An `open` the resolver could not name-key (EX-2: a project open, an
        // assembly-module / `open type`, an opaque path) defers wholesale too —
        // exactly as the pre-EX-2 presence gate did for *any* `open`.
        let unknowable_open = ExtensionScope {
            opens_unknowable: true,
            ..ExtensionScope::default()
        };
        assert!(
            !unknowable_open.absent(&env, "Substring", false),
            "an un-name-keyable open defers every name (presence-based, EX-2)"
        );
        assert!(
            !unknowable_open.absent(&env, "NoSourceDeclaresThis", false),
            "…including a name no source declares"
        );
    }
}
