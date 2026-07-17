//! Resolution of dotted paths into referenced assemblies.

use borzoi_cst::syntax::SyntaxToken;
use rowan::TextRange;

use crate::assembly_env::{EntityHandle, StaticLookup};

use super::id_text;
use super::model::{DeferredReason, Resolution};
use super::state::{AssemblyPath, Resolver, ShadowVeto, TieredResolution, TypePathReading};

impl<'a> Resolver<'a> {
    /// Compute — *without recording* — how a dotted path resolves into the
    /// referenced assemblies, under an opened-namespace `prefix` (empty for a
    /// directly fully-qualified path). Pure, so the caller can compare
    /// candidates across several opens, and — crucially — distinguish a path F#
    /// resolves *within the project* ([`AssemblyPath::ProjectShadowed`], which
    /// must defer rather than fall through to another open) from a genuine
    /// non-match ([`AssemblyPath::NoMatch`], which may try the next open).
    ///
    /// On a hit it records [`Resolution::Entity`] at the rooting type's segment
    /// (and each nested-type segment) and [`Resolution::Member`] at the
    /// whole-path range — mirroring how FCS reports the rightmost long-id item
    /// spanning the whole path and intermediate items at their own segment.
    /// `prefix` segments are implicit (no source token); a full-path index `i`
    /// maps to source token `segments[i - prefix.len()]` for `i >= prefix.len()`.
    /// The rooting type's name must be a source segment, so an opened path that
    /// is *itself* a type (`open type`) yields `NoMatch`. Generic arity is `0`.
    pub(super) fn assembly_path_records(
        &self,
        prefix: &[String],
        segments: &[SyntaxToken],
    ) -> AssemblyPath<Vec<(TextRange, Resolution)>> {
        let base = prefix.len();
        let mut names: Vec<String> = prefix.to_vec();
        names.extend(segments.iter().map(|t| id_text(t.text()).to_string()));
        let n = names.len();

        // Decline a path F# resolves within the project (see
        // [`Self::path_is_project_shadowed`]), searched before referenced
        // assemblies.
        if self.path_is_project_shadowed(&names) {
            return AssemblyPath::ProjectShadowed;
        }

        // Longest prefix whose `(namespace, name)` is a public top-level type
        // *and* whose type name is a source segment (`k >= base`).
        let Some((k, type_handle)) = (base..n).rev().find_map(|k| {
            self.assemblies
                .lookup_type(&names[..k], &names[k], 0)
                .filter(|&handle| self.assemblies.is_public(handle))
                .map(|handle| (k, handle))
        }) else {
            return AssemblyPath::NoMatch;
        };

        // A type-abbreviation marker: the name binds (FCS chases the
        // abbreviation to its target, e.g. `S.Format` where `type S =
        // System.String`), but the target is unmodelled — we can neither
        // resolve the member tail nor safely let a lower-priority reading
        // take the path. Shadow-defer (D5: defer, never a wrong target).
        if self.assemblies.is_abbreviation(type_handle) {
            return AssemblyPath::ProjectShadowed;
        }

        let mut recs: Vec<(TextRange, Resolution)> = Vec::new();
        let deferred = Resolution::Deferred(DeferredReason::QualifiedAccess);
        // Namespace qualifier segments that are in the *source* (indices
        // `base..k`) are modeled uses we cannot resolve — defer, never drop.
        for seg in &segments[..(k - base)] {
            recs.push((seg.text_range(), deferred));
        }
        recs.push((
            segments[k - base].text_range(),
            Resolution::Entity(type_handle),
        ));

        // Walk the segments past the rooting type: nested types extend the
        // chain; a public *static* member ends it (a type-qualified path
        // resolves only static members; FCS reports the member spanning the
        // whole path). `owns_path` records whether the reading captures the whole
        // path — see [`AssemblyPath::Resolved`]; it stays `true` unless a segment
        // names nothing on its parent (a genuinely-absent tail).
        let mut parent = type_handle;
        let mut i = k + 1;
        let mut owns_path = true;
        while i < n {
            let src = &segments[i - base];
            if let Some(child) = self
                .assemblies
                .nested(parent, &names[i], 0)
                .filter(|&h| self.assemblies.is_public(h))
            {
                // A nested abbreviation marker: same defer as the rooting
                // case above.
                if self.assemblies.is_abbreviation(child) {
                    return AssemblyPath::ProjectShadowed;
                }
                recs.push((src.text_range(), Resolution::Entity(child)));
                parent = child;
                i += 1;
            } else {
                match self.assemblies.static_lookup(parent, &names[i]) {
                    StaticLookup::Resolved(idx) => {
                        let whole = TextRange::new(
                            segments[0].text_range().start(),
                            src.text_range().end(),
                        );
                        recs.push((whole, Resolution::Member { parent, idx }));
                        i += 1;
                        break;
                    }
                    // The name is *occupied*, but we cannot name a target: an
                    // overloaded public static (we do not model overload resolution),
                    // a metadata ambiguity, an undecidable augmentation on a
                    // pickle-less image, or a member FCS's lookup reaches but a
                    // qualified path cannot select (an instance-only member, an
                    // inherited static, an unknowable base chain — see
                    // [`AssemblyEnv::static_lookup`]). Defer the member yet keep
                    // `owns_path` — the rooting type captures the whole reference, so
                    // a lower-priority tier must not re-root the path elsewhere and
                    // resolve it to some *other* module's same-named member (review
                    // round 3). Mirrors the unqualified path
                    // ([`Self::open_type_statics`]), where such a name shadows by
                    // position and defers.
                    StaticLookup::Uncertain => {
                        recs.push((src.text_range(), deferred));
                        i += 1;
                        break;
                    }
                    // The segment names nothing FCS's qualified lookup can reach on its
                    // parent — not on the entity, not through its base chain — so the
                    // tail is genuinely absent, this reading only *partially* matches,
                    // and a lower tier that resolves the whole path may supersede it.
                    // `Absent` is exactly that condition and nothing else, which is why
                    // the fall-through can read it off the lookup directly rather than
                    // re-deriving it from a second ownership predicate that could
                    // disagree (review rounds 3 and 4 were that disagreement, twice).
                    StaticLookup::Absent => {
                        owns_path = false;
                        break;
                    }
                }
            }
        }
        // Anything left — an unresolvable tail, or member access on the static
        // member's result — is modeled-but-unresolved: deferred, not dropped.
        for seg in &segments[(i - base)..] {
            recs.push((seg.text_range(), deferred));
        }
        AssemblyPath::Resolved {
            payload: recs,
            owns_path,
        }
    }

    /// Every opened-namespace prefix in scope, in strict F# priority order:
    /// opens **latest-first** (F# is latest-open-wins, not ambiguity), and within
    /// one open its readings as the group orders them (relative before merged
    /// root — see [`OpenGroup`](super::state::OpenGroup)). The one walk order for
    /// every consumer of [`imports`](Self::imports), so a consumer cannot invert
    /// the precedence.
    pub(super) fn open_reading_prefixes(&self) -> impl Iterator<Item = &[String]> {
        self.imports
            .iter()
            .rev()
            .flat_map(|open| open.readings.iter().map(Vec::as_slice))
    }

    /// Every prefix a dotted path may be read under, in strict F# precedence
    /// order — the readings [`Self::resolve_assembly_path_tiered`] walks:
    /// 1. **opens** ([`Self::open_reading_prefixes`]);
    /// 2. the **current enclosing namespace** ([`Self::enclosing_namespace`]):
    ///    FS0039 — the current namespace's child, never an ancestor, never a
    ///    module segment past it;
    /// 3. **root / as-written** (the empty prefix).
    ///
    /// `pub(super)` so the unmodelled-open guard in `lookup.rs` iterates the
    /// same sequence — a tier added here must be visible to that guard too.
    pub(super) fn assembly_prefixes_by_priority(&self) -> impl Iterator<Item = &[String]> {
        const ROOT: &[String] = &[];
        self.open_reading_prefixes()
            .chain(Some(self.enclosing_namespace()).filter(|e| !e.is_empty()))
            .chain(std::iter::once(ROOT))
    }

    /// Walk F#'s referenced-assembly name-lookup precedence — every reading in
    /// [`Self::assembly_prefixes_by_priority`] order — and decide the path's fate
    /// by one uniform rule:
    ///
    /// - the first reading that resolves the **whole** path wins
    ///   ([`TieredResolution::Resolved`], for the caller to [`Self::apply`]);
    /// - the first **project-shadowed** reading defers
    ///   ([`TieredResolution::ShadowDeferred`]): a project entity owns the name at
    ///   that priority and may satisfy the whole path invisibly (sema does not
    ///   model project types / nested-module members), so no lower-priority
    ///   reading — and no held *partial* — may be applied over it. FCS-pinned both
    ///   ways: `open Ns; open Demo.Sub; (x: Calc.Inner)` with a project
    ///   `Ns.Calc.Inner` binds the project type over the later open's partial
    ///   `Demo.Sub.Calc` (R7-A), and the same holds when the completing project
    ///   entity sits at the *enclosing-namespace* or *root* priority instead of an
    ///   open (`namespace Demo; open Demo.Sub; (x: Calc.Inner)` with a preceding
    ///   `module Calc = type Inner` binds `Demo.Calc.Inner`);
    /// - a **partial** reading (rooting type found, tail genuinely absent — its
    ///   [`owns_path`](AssemblyPath::Resolved) is `false`) is *held*: a lower
    ///   priority may still
    ///   resolve the whole path and F# prefers the reading that does
    ///   (`open Demo; open Sub; Calc.Answer` is `Demo.Calc.Answer`: the latest
    ///   open's `Sub.Calc` lacks `Answer`, so the earlier `Demo.Calc` wins). If
    ///   the walk ends with no complete reading and no shadow, the
    ///   highest-priority partial is the result (`Demo.Calc.Nope` — the type
    ///   resolves, the bad member defers), so a path that already worked never
    ///   under-resolves;
    /// - nothing at all → [`TieredResolution::NoMatch`].
    ///
    /// This is the one place the precedence walk lives; both the *type* path
    /// ([`Self::resolve_type_path`]) and the *value/member* path
    /// ([`Self::resolve_long_ident`]) call it, passing their own leaf
    /// record-generator (`assembly_type_path_core` — arity-aware, no member
    /// tail, token-free; or `assembly_path_records` — a trailing static member
    /// becomes a `Member`).
    ///
    /// `as_written_vetoes_opens` — whether a **project-shadowed as-written**
    /// reading defers *before* the opens are even tried:
    /// - **Value/member path → true.** A project-bound head — a lexically-in-scope
    ///   nested module / local, or a value prefix — captures the whole reference;
    ///   an `open` cannot redirect an already-project-rooted head, so we defer
    ///   (the `assembly_path_records` soundness tripwire; the `nested_module_*`
    ///   shadow tests).
    /// - **Type path → false.** The only single-name project binder that reaches
    ///   the type as-written reading is a **module** (a same-file `type` is
    ///   resolved earlier by [`Self::resolve_in_file_type_path`]), and a module is
    ///   not a type, so it does not capture a *type* reference: `module Calc;
    ///   open Demo; (x : Calc)` is the assembly type `Demo.Calc` via the open
    ///   (FCS). The as-written reading then keeps its ordinary lowest-priority
    ///   place in the walk.
    ///
    /// `shadow_at` — the per-prefix shadow verdict ([`ShadowVeto`]), checked
    /// *inside* the per-tier loop — not before or after the whole walk —
    /// which is what lets a higher-priority shadow risk win over a
    /// lower-priority real match, and a real match at equal-or-higher
    /// priority than any shadow risk win over it in turn. A
    /// [`ShadowVeto::Preemptive`] verdict (exact metadata) vetoes even a
    /// same-tier real match — FCS-probed: `namespace Ns; type Foo = …;
    /// [<AutoOpen>] module Auto = type Foo = …` then `open Ns; (x : Foo)`
    /// binds `Ns.Auto.Foo`, not the direct `Ns.Foo` (found by review, round
    /// 6, on `docs/completed/r2-annotation-typing-plan.md`). A
    /// [`ShadowVeto::OnNoMatch`] verdict (coarse, name-blind) applies only
    /// once the tier's own lookup comes up empty. The value path passes
    /// `|_| ShadowVeto::None`: sema already enumerates an auto-open module's
    /// *values* (only its nested *types* are unmodelled), and it has no
    /// coarse unmodelled-shadow source once past the
    /// `unmodelled_open_active` guard its caller applies first. Once a
    /// fallback reading is held it already outranks anything at or below the
    /// current tier — shadow risk included — so the verdict is not consulted
    /// again.
    pub(super) fn resolve_assembly_path_tiered<R>(
        &self,
        records: impl Fn(&[String]) -> AssemblyPath<R>,
        as_written_vetoes_opens: bool,
        shadow_at: impl Fn(&[String]) -> ShadowVeto,
    ) -> TieredResolution<R> {
        // The veto's root reading is held and consumed when the walk reaches the
        // ROOT tier (the final, empty prefix), instead of recomputing it —
        // `records` is pure, and the root reading is the common case's most
        // expensive one to duplicate.
        let mut root = as_written_vetoes_opens.then(|| records(&[]));
        if matches!(root, Some(AssemblyPath::ProjectShadowed)) {
            return TieredResolution::ShadowDeferred;
        }

        // The highest-priority partial reading seen so far; the result only if
        // the whole walk ends with no owning reading and no project shadow. A
        // reading is *owning* (`owns_path`) iff it captures the whole path — a
        // nested-type chain, a unique static member, or an overload set the type
        // owns but cannot uniquely select (see [`AssemblyPath::Resolved`]). Once
        // set, a held fallback already outranks anything at or below the
        // current tier — shadow risk included — so the verdict is not
        // consulted once a fallback has been seen.
        let mut fallback: Option<R> = None;

        for prefix in self.assembly_prefixes_by_priority() {
            let veto = if fallback.is_none() {
                shadow_at(prefix)
            } else {
                ShadowVeto::None
            };
            if veto == ShadowVeto::Preemptive {
                return TieredResolution::ShadowDeferred;
            }
            let reading = match root.take() {
                // Only the ROOT tier has an empty prefix (a reading/namespace
                // prefix is never empty), so the held value is consumed exactly
                // there.
                Some(r) if prefix.is_empty() => r,
                other => {
                    root = other;
                    records(prefix)
                }
            };
            match reading {
                AssemblyPath::Resolved { payload, owns_path } => {
                    if owns_path {
                        return TieredResolution::Resolved(payload);
                    }
                    fallback.get_or_insert(payload);
                }
                AssemblyPath::ProjectShadowed => return TieredResolution::ShadowDeferred,
                AssemblyPath::NoMatch => {
                    if veto == ShadowVeto::OnNoMatch {
                        return TieredResolution::ShadowDeferred;
                    }
                }
            }
        }
        match fallback {
            Some(payload) => TieredResolution::Resolved(payload),
            None => TieredResolution::NoMatch,
        }
    }

    /// The type-position sibling of [`Self::assembly_path_records`],
    /// **token-free**: resolve a dotted path — the source segment *names*
    /// `segments`, `idText`-normalised, under an opened-namespace `prefix` — to
    /// a referenced-assembly **type**, carrying the generic `arity` written at
    /// the use. Like its expression sibling it marks [`Resolution::Entity`] at
    /// the rooting type's segment and each nested-type segment, and
    /// [`Resolution::Deferred`] at namespace-qualifier and unresolvable-tail
    /// segments — but **keyed by segment index** rather than a source range, so
    /// a path with no source tokens can be resolved through the same walk (the
    /// synthesised `…Attribute` attribute candidate,
    /// `docs/extension-scope-enumeration-plan.md` §2(d)). It has **no
    /// static-member tail** (a type reference ends in a type, never a member),
    /// and the lookup is arity-aware.
    ///
    /// The arity applies to the path's **final** segment (the type actually
    /// named); an *enclosing* type along the path is keyed at arity 0. A generic
    /// *encloser* (`Outer<'a>.Inner`) therefore under-resolves — a known gap that
    /// stays sound (it never records a wrong entity, only declines).
    pub(super) fn assembly_type_path_core(
        &self,
        prefix: &[String],
        segments: &[String],
        arity: usize,
    ) -> AssemblyPath<TypePathReading> {
        let base = prefix.len();
        let mut names: Vec<String> = prefix.to_vec();
        names.extend(segments.iter().cloned());
        let n = names.len();

        // Decline a path F# resolves to a project **type/module** ahead of the
        // referenced assemblies. This is the *type-namespace* check — a project
        // *value* of the same name does NOT shadow a type in type position
        // (`module Demo; let Thing = 1` elsewhere does not stop `x : Demo.Thing`
        // resolving to the assembly type), so it must not pull in the value-space
        // shadowing that the expression sibling's `path_is_project_shadowed` adds.
        if self.path_is_project_type_shadowed(&names) {
            return AssemblyPath::ProjectShadowed;
        }

        // Longest prefix `[..k]` (with `k >= base`, a source segment) whose
        // `(namespace, name)` is a public top-level type. The arity is applied to
        // the final segment only — an encloser in the path is keyed at arity 0.
        let arity_at = |k: usize| if k == n - 1 { arity } else { 0 };
        let Some((k, type_handle)) = (base..n).rev().find_map(|k| {
            self.assemblies
                .lookup_type(&names[..k], &names[k], arity_at(k))
                .filter(|&handle| self.assemblies.is_public(handle))
                .map(|handle| (k, handle))
        }) else {
            return AssemblyPath::NoMatch;
        };

        // A type-abbreviation *marker* (a metadata-invisible F# abbreviation
        // surfaced name-only from the signature pickle): the name is really
        // taken — FCS binds the abbreviation here — but its target type is
        // not modelled, so resolving any reading through it would either
        // fabricate a target or, worse, let a lower-priority reading win.
        // Shadow-defer the whole path instead (D5: defer, never a wrong
        // target).
        if self.assemblies.is_abbreviation(type_handle) {
            return AssemblyPath::ProjectShadowed;
        }

        let mut idx_recs: Vec<(usize, Resolution)> = Vec::new();
        let deferred = Resolution::Deferred(DeferredReason::QualifiedAccess);
        // Source namespace-qualifier segments (indices `base..k`) are modeled uses
        // we cannot resolve — defer, never drop.
        for idx in 0..(k - base) {
            idx_recs.push((idx, deferred));
        }
        idx_recs.push((k - base, Resolution::Entity(type_handle)));

        // Walk the segments past the rooting type as public nested types; the
        // final segment carries `arity`, each intermediate encloser arity 0.
        // `owns_path` (see [`AssemblyPath::Resolved`]) holds unless a segment
        // names no public nested type — a type path has no member tail, so that
        // absent-segment case is the only way it fails to capture the whole path.
        let mut parent = type_handle;
        let mut i = k + 1;
        let mut owns_path = true;
        while i < n {
            if let Some(child) = self
                .assemblies
                .nested(parent, &names[i], arity_at(i))
                .filter(|&h| self.assemblies.is_public(h))
            {
                // A nested abbreviation marker (`Lib.Auto.Foo` where `Foo` is
                // a module-scoped abbreviation): same defer as the rooting
                // case above — the name binds, the target is unmodelled.
                if self.assemblies.is_abbreviation(child) {
                    return AssemblyPath::ProjectShadowed;
                }
                idx_recs.push((i - base, Resolution::Entity(child)));
                parent = child;
                i += 1;
            } else {
                owns_path = false;
                break;
            }
        }
        // An unresolvable tail (a nested type we don't model, or a non-type
        // segment) is modeled-but-unresolved: defer, never drop.
        for idx in (i - base)..segments.len() {
            idx_recs.push((idx, deferred));
        }
        AssemblyPath::Resolved {
            payload: TypePathReading {
                idx_recs,
                // The whole-path type, exactly when the reading owns the path
                // (`parent` walked to the final segment); a partial reading has
                // no whole-path type to name.
                leaf: owns_path.then_some(parent),
            },
            owns_path,
        }
    }

    pub(super) fn apply(&mut self, recs: Vec<(TextRange, Resolution)>) {
        for (range, res) in recs {
            self.record(range, res);
        }
    }

    /// The accessible *type* `path` opens, if it names one (an F# module compiles
    /// to a type, as does a class) rather than a namespace — i.e. the whole path
    /// resolves to a **public** type in the assembly env, top-level **or
    /// nested**. A *plain* `open` of such a type does not import its statics
    /// unqualified (only `open type` does — see [`Self::open_type_statics`]); the
    /// caller uses this only to classify the open — a *module* makes bare-name
    /// resolution opaque ([`Self::opaque_value_open`]), a *class* brings nothing
    /// unqualified — and either way to suppress the (namespace) opens we model for
    /// *qualified* paths, since the opened type's nested types are unmodelled.
    /// `None` for a namespace path.
    ///
    /// The `is_public` filter mirrors [`Self::assembly_path_records`]: an
    /// `internal` type F# cannot open cross-assembly is *not* a type open, so it
    /// must not suppress other valid opens in the file. (An inaccessible path
    /// then falls through to being recorded as a namespace prefix, which simply
    /// never matches — a no-op — since a type is not a namespace.)
    ///
    /// Walks like a fully-qualified path: the longest top-level `(namespace,
    /// name)` prefix that is a public type, then the remaining segments as
    /// public nested types — a type iff that consumes the whole path.
    pub(super) fn opened_assembly_type(&self, path: &[String]) -> Option<EntityHandle> {
        let n = path.len();
        let (k, mut handle) = (0..n).rev().find_map(|k| {
            self.assemblies
                .lookup_type(&path[..k], &path[k], 0)
                .filter(|&h| self.assemblies.is_public(h))
                .map(|h| (k, h))
        })?;
        for seg in &path[k + 1..] {
            handle = self
                .assemblies
                .nested(handle, seg, 0)
                .filter(|&h| self.assemblies.is_public(h))?; // not an accessible nested type
        }
        Some(handle)
    }

    /// The **assembly module** `path` names, or `None` — the entity an
    /// `open <assembly module>` enumerates ([`Resolver::open_interpretations`]).
    ///
    /// A `[<RequireQualifiedAccess>]` module **is** one. Opening it is an *error*
    /// (FS0892), but FCS still enters its contents into the name environment — the
    /// original Q5 probe misread a lone FS0892 as "imports nothing", when in fact the
    /// bare use that followed resolved fine and produced no FS0039 (re-probed after the
    /// review; `docs/assembly-module-open-plan.md` Q5 is corrected). Dropping it from
    /// the walk would be a *wrong target*, not a deferral: with `open Prefix` in scope,
    /// `open M` where `Prefix.M` is RQA and a root `M` exists would bind the root `M`'s
    /// values where FCS binds `Prefix.M`'s. Reporting FS0892 is a Phase-4 concern
    /// ([`AssemblyEnv::is_require_qualified_access`] is the signal).
    pub(super) fn opened_assembly_module(&self, path: &[String]) -> Option<EntityHandle> {
        self.opened_assembly_modules(path).into_iter().next()
    }

    /// **Every** assembly module `path` names — one per referenced assembly that
    /// exposes the FQN. FCS merges them (`open Dup.M` with two assemblies exposing
    /// `Dup.M` imports the unique values of both; a collision binds the
    /// later-referenced one — fsi-verified), so opening only the first would lose the
    /// other's values and could bind a collision to the wrong assembly.
    ///
    /// Same walk as [`Self::opened_assembly_type`] — longest top-level
    /// `(namespace, name)` prefix, then nested types — but branching over *all* roots
    /// at that prefix ([`AssemblyEnv::public_entities_named`]) rather than the
    /// first-wins index.
    pub(super) fn opened_assembly_modules(&self, path: &[String]) -> Vec<EntityHandle> {
        let n = path.len();
        let mut out: Vec<EntityHandle> = Vec::new();
        // **Every** split, not just the longest. One assembly may expose `A.B.C` as a
        // top-level type in namespace `A.B` (the `module A.B.C` shape) while another
        // nests it — root module `A` with nested `B`, nested `C` — and FCS merges both.
        // Stopping at the first split that yields roots would silently drop the other
        // encoding's module: its unique values would vanish, and a colliding value would
        // look unique and bind the wrong assembly (review round 7).
        for k in (0..n).rev() {
            let roots = self.assemblies.public_entities_named(&path[..k], &path[k]);
            if roots.is_empty() {
                continue;
            }
            for root in roots {
                let mut handle = Some(root);
                for seg in &path[k + 1..] {
                    // A module path descends through *modules*: `nested` would hand back
                    // the companion **type** where a type and a suffixed module share a
                    // name (`type Tagged` + `module Tagged` ⇒ `TaggedModule`), and
                    // `open Demo.Outer.Tagged` imports the module (review round 6).
                    handle = handle.and_then(|h| self.assemblies.nested_module(h, seg));
                }
                if let Some(h) = handle.filter(|&h| self.assemblies.is_module(h))
                    && !out.contains(&h)
                {
                    out.push(h);
                }
            }
        }
        out
    }

    /// Resolve the *type* an `open type T` brings into scope, following F#'s
    /// **type-name** resolution precedence (FCS-verified). `T` may be shortened
    /// by an earlier `open`, written relative to the enclosing namespace, or
    /// written fully-qualified, resolved in that precedence order:
    ///
    /// 1. **explicit opens** — `open Demo; open type Calc` ≡ `open type Demo.Calc`;
    /// 2. **enclosing namespace/module** nesting, innermost first — `open type
    ///    Calc` in `namespace Demo` binds `Demo.Calc`;
    /// 3. **root / fully-qualified** — `open type Demo.Calc`, or a bare root type.
    ///
    /// An explicit `open` outranks the enclosing namespace, which outranks the
    /// root: in `namespace Demo` with `open Demo.Sub`, `open type Calc` binds
    /// `Demo.Sub.Calc` (the open), not `Demo.Calc`; and `open Demo; open type
    /// Calc` binds `Demo.Calc`, not a root `Calc`.
    ///
    /// Shadowing uses the *type-namespace* check
    /// ([`Self::path_is_project_type_shadowed`]) — a project value of the same
    /// name does not shadow a type. `None` (an *opaque* open: bare-name resolution
    /// stays conservative) when the target is project-shadowed, names no
    /// accessible assembly type, or is ambiguous across distinct opens (F# breaks
    /// that by latest-open precedence, which we do not model, so we decline rather
    /// than guess). The explicit-open and enclosing-namespace tiers are suppressed
    /// while an [`unmodelled_open_active`](Self::unmodelled_open_active) prior open
    /// could shorten the name through a path we cannot see; a fully-qualified
    /// path (tier 3) needs no open, so it is still honoured.
    pub(super) fn opened_type_target(&self, path: &[String]) -> Option<EntityHandle> {
        // The shortening tiers (explicit opens, enclosing namespace) only when no
        // unmodelled open could invisibly provide the name.
        if !self.unmodelled_open_active {
            // Tier 1 — opens, in the shared [`Self::open_reading_prefixes`] order
            // (latest-open-first; within an open relative-before-root), mirroring
            // [`Self::resolve_assembly_path_tiered`]. So in `namespace Demo; open
            // Sub`, an `open type` target only in the root `Sub` (`RootOnly`)
            // resolves through the open's root reading — without it the open would
            // wrongly go opaque and suppress later opened statics — while a
            // colliding name takes the relative `Demo.Sub`. The latest open with a
            // match wins.
            for prefix in self.open_reading_prefixes() {
                let mut full = prefix.to_vec();
                full.extend_from_slice(path);
                if self.path_is_project_type_shadowed(&full) {
                    return None; // an open routes it into project territory
                }
                if let Some(handle) = self.opened_assembly_type(&full) {
                    return Some(handle);
                }
            }
            // Tier 2 — enclosing namespace/module nesting, innermost first. The
            // assembly lookup runs *before* the shadow check because every prefix
            // is trivially "rooted at the current module" (the innermost prefix
            // *is* it), which the type-shadow check reports — that only means
            // *decline* when an assembly type actually sits at this path and a
            // project entity shadows it; otherwise we keep walking outward.
            for k in (1..=self.container_path.len()).rev() {
                let mut full = self.container_path[..k].to_vec();
                full.extend_from_slice(path);
                if let Some(handle) = self.opened_assembly_type(&full) {
                    return (!self.path_is_project_type_shadowed(&full)).then_some(handle);
                }
            }
        }
        // Tier 3 — the path as written, from the root (a fully-qualified path, or
        // a bare root-namespace type). Lowest precedence, so a shortenable name
        // resolves through tiers 1–2 first.
        //
        // Suppressed for a *bare* single-segment name inside an enclosing
        // namespace: a project type of that name declared in the namespace would
        // shadow a root type, but we do not index project *types* across files, so
        // a cross-file `namespace Demo; type Calc` is invisible here. Resolving the
        // root type could therefore be wrong — decline (defer) rather than guess.
        let bare_in_namespace = path.len() == 1 && !self.container_path.is_empty();
        if !bare_in_namespace && let Some(handle) = self.opened_assembly_type(path) {
            return (!self.path_is_project_type_shadowed(path)).then_some(handle);
        }
        None
    }

    /// Whether `path` names (or sits under) an in-project **module** — an F#
    /// module's `let` bindings enter unqualified scope when it is opened; a
    /// namespace's do not. The project-module predicate for
    /// [`resolved_project_module`](Self::resolved_project_module): a plain `open M`
    /// whose (tier-resolved) path satisfies this enumerates M's direct values into
    /// the frame ([`Self::open_module_values`]) rather than treating the open as a
    /// namespace prefix or assembly type. Covers project modules from this file
    /// (top-level [`Self::module_paths`] and nested) and earlier Compile-order
    /// files (via [`Preceding`]).
    ///
    /// The *nested*-module checks match a **prefix** (`open Calc.Inner` where
    /// `Calc` — or `Calc.Inner` — is a nested module): opening anything under a
    /// project module still brings unmodelled values. A namespace path matches
    /// none of these, so an `open <namespace>` stays non-opaque. (Top-level
    /// `module_paths` stays an *exact* match because it also holds the file's
    /// `namespace` headers, which must not make an `open <namespace>` opaque.)
    ///
    /// [`Preceding`]: ProjectItems
    pub(super) fn open_imports_project_values(&self, path: &[String]) -> bool {
        let under_any = |paths: &[Vec<String>]| {
            paths
                .iter()
                .any(|p| !p.is_empty() && path.starts_with(p.as_slice()))
        };
        self.module_paths.iter().any(|p| p == path)
            || under_any(&self.nested_module_locals)
            || under_any(&self.nested_module_exports)
            || self.preceding.is_exact_project_module(path)
            || self.preceding.is_rooted_at_nested_module(path)
    }
}
