//! Resolution of dotted paths into referenced assemblies.

use borzoi_assembly::EntityKind;
use borzoi_cst::syntax::SyntaxToken;
use rowan::TextRange;

use crate::assembly_env::{EntityHandle, StaticLookup};

use super::id_text;
use super::model::{DeclineCause, DeclineSite, DeclineTier, DeferredReason, Resolution};
use super::state::{AssemblyPath, Resolver, ShadowVeto, TieredResolution, TypePathReading};

/// One candidate that **owns** a path at one rooting position — the unit
/// [`Resolver::unopposed_owner`] decides between.
struct PositionOwner {
    /// The rooting position (the index of the segment the reading rooted at);
    /// longer is higher priority.
    position: usize,
    handle: EntityHandle,
    payload: Vec<(TextRange, Resolution)>,
}

/// What one rooting position yielded: the candidates that owned the path, in
/// candidate order, and the highest-priority partial for when none did.
struct PositionReading {
    owners: Vec<PositionOwner>,
    partial: Option<Vec<(TextRange, Resolution)>>,
}

impl<'a> Resolver<'a> {
    /// Compute — *without recording* — how a dotted path resolves into the
    /// referenced assemblies, under an opened-namespace `prefix` (empty for a
    /// directly fully-qualified path). Pure, so the caller can compare
    /// candidates across several opens, and — crucially — distinguish a path
    /// something already holds ([`AssemblyPath::Occupied`], whose payload names
    /// the occupant, and which must defer rather than fall through to another
    /// open) from a genuine non-match ([`AssemblyPath::NoMatch`], which may try
    /// the next open).
    ///
    /// On a hit it records [`Resolution::Entity`] at the rooting type's segment
    /// (and each nested-type segment) and [`Resolution::Member`] at the
    /// whole-path range — mirroring how FCS reports the rightmost long-id item
    /// spanning the whole path and intermediate items at their own segment.
    /// `prefix` segments are implicit (no source token); a full-path index `i`
    /// maps to source token `segments[i - prefix.len()]` for `i >= prefix.len()`.
    /// The rooting type's name must be a source segment, so an opened path that
    /// is *itself* a type (`open type`) yields `NoMatch`.
    ///
    /// A value-path head writes **no** generic arity — a type application cannot
    /// appear in expression position — but that does not restrict the rooting to
    /// arity 0: FCS infers the arguments (`FS1125`) and binds the generic type,
    /// so the rooting is chosen from the whole candidate set at the name and by
    /// which member of it owns the path
    /// ([`Self::rooting_candidates`], [`Self::rooting_reading`]).
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
        // [`Self::project_shadow_cause`]), searched before referenced
        // assemblies. A path shadowed *only* by the current module's own name is
        // a self-qualifier FCS does not bind into the current module (`M.x` inside
        // `M` is FS0039), so an `open` / implicit `[<AutoOpen>]` may still supply
        // it (`List.fold` inside `module List` → `Microsoft.FSharp.Collections`):
        // it defers at the *root* but must not preempt the opens tier.
        if let Some(cause) = self.project_shadow_cause(&names) {
            // Two questions, and they are not the same one:
            //
            // - is the **written** path a self-qualifier? A fact about what the
            //   user wrote, so it is asked of the source slice. The
            //   prefix-expanded `N.List.rev` for a written `List.rev` has head
            //   `N`, a namespace segment in no module chain, so asking it there
            //   classifies the reading `Occupied` and preempts the very
            //   opens the `module List` augmentation idiom falls through to.
            // - is *this reading's* shadow that same self module? Only then may
            //   the reading be held rather than deferred at. A prefixed reading
            //   can be shadowed by something else entirely — with a preceding
            //   file's `Q.List.rev` and `namespace X; open Q; module List`, the
            //   `Q` reading is shadowed by a real project member FCS binds, and
            //   holding it would let the walk fall through to FSharp.Core.
            //   The self module's own reading is exactly the reconstructed self
            //   path, so that equality is the test; a self-qualifier the
            //   reconstruction cannot place (`rooted_at_current_module` alone)
            //   is conservatively a plain project shadow.
            //
            // For the as-written reading the two slices are equal and the
            // expanded path is the source path, so only prefixed readings change.
            let source = &names[base..];
            let shadow_is_the_self_module = base == 0
                || self
                    .self_qualified_member_path(source)
                    .is_some_and(|reconstructed| reconstructed == names);
            if !(shadow_is_the_self_module && self.self_module_shadow_only(source)) {
                return AssemblyPath::Occupied(cause);
            }
            // A self-qualifier declines only where there is something to be
            // uncertain *about*. The verdict exists because an assembly entity at
            // this reading may still answer the path — the project module and a
            // referenced one can share an FQN, and `Calc.Zero()` inside
            // `Demo.Calc` binds the assembly's `Demo.Calc.Zero` — which is a
            // question the walk cannot decide. Where the assemblies hold no
            // rooting position at all, there is no such reading: the self module
            // is the only occupant, F# does not bind it (FS0039), and the reading
            // is an ordinary no-match that lets the walk carry on. Declining
            // instead would be an absence we never looked for, and it costs the
            // `module List = …` augmentation idiom its fall-through to
            // `Microsoft.FSharp.Collections` at every tier the self module reaches
            // before the opens.
            return if self.any_rooting_position(&names, base) {
                AssemblyPath::SelfModuleShadowed
            } else {
                AssemblyPath::NoMatch
            };
        }

        // Walk **every** rooting position — each prefix whose
        // `(namespace, name)` names a public top-level entity and whose name is
        // a source segment (`k >= base`) — collect every reading that owns the
        // whole path, and commit only if the owner is **unique**.
        //
        // Uniqueness is the invariant, not an optimisation. The head names a
        // candidate *set* at each of several positions, so "the first owner I
        // found" is decided by walk order, and every review round found a shape
        // where that order was not FCS's: a companion module beside its type, a
        // generic type family (`C<'T>` and `C<'T,'U>` both carrying the tail,
        // where the source's written type arguments — which do not reach this
        // walk — pick between them), and one DLL's type at a longer position
        // against another's module at a shorter one, which FCS decides by
        // reference order. Committing only an unopposed owner makes all of them
        // deferrals instead of guesses (codex review rounds 1-4).
        //
        // The one contested shape that still commits is the fsi-measured
        // tie-break it was measured for: a module and its **own companion
        // types** — same position, same DLL — where F# binds the module
        // (`Holder.Tail` with the name on both).
        let mut partial: Option<Vec<(TextRange, Resolution)>> = None;
        let mut owners: Vec<PositionOwner> = Vec::new();
        let mut any_rooting = false;
        for k in (base..n).rev() {
            let candidates = self.rooting_candidates(&names[..k], &names[k]);
            if candidates.is_empty() {
                continue;
            }
            any_rooting = true;
            match self.rooting_at(&names, segments, base, k, &candidates) {
                // A position that cannot be *named* — a merged rooting, an
                // opaque abbreviation, a project shadow — decides the reading
                // where it sits, because rooting *shorter* over it would commit
                // exactly where FCS's own lookup is undecidable for us. Unless
                // a **longer** position already owned the path: that one is
                // higher priority, and an undecidable candidate below it never
                // gets a say.
                Err(verdict) => {
                    if owners.is_empty() {
                        return verdict;
                    }
                }
                Ok(reading) => {
                    owners.extend(reading.owners);
                    if let Some(p) = reading.partial {
                        partial.get_or_insert(p);
                    }
                }
            }
        }
        if !any_rooting {
            return AssemblyPath::NoMatch;
        }
        if let Some(owner) = self.unopposed_owner(&owners) {
            return AssemblyPath::Resolved {
                payload: owner,
                owns_path: true,
            };
        }
        if !owners.is_empty() {
            return AssemblyPath::ContestedRooting;
        }
        // A rooting exists and nothing owned the path, so the reading matches
        // only partially — the fall-through the tier walk holds and a
        // lower-priority *prefix* may supersede.
        AssemblyPath::Resolved {
            payload: partial.unwrap_or_else(|| {
                segments
                    .iter()
                    .map(|seg| {
                        (
                            seg.text_range(),
                            Resolution::Deferred(DeferredReason::QualifiedAccess),
                        )
                    })
                    .collect()
            }),
            owns_path: false,
        }
    }

    /// Whether any **rooting position** exists for this reading — the condition
    /// [`Self::assembly_path_records`]'s walk calls `any_rooting`, asked without
    /// walking the readings themselves.
    ///
    /// `false` is the strong answer: no split of `names` at or past `base` names
    /// a public top-level entity, so no candidate exists for the walk to read
    /// through and the reading cannot resolve, hold a partial, or be contested.
    /// It is therefore the one place a caller may treat "we found nothing" as
    /// "there is nothing", rather than as "we did not look".
    ///
    /// Which is why a **dropped TypeDef** counts as a rooting position. The
    /// projector records the namespace it lost a type from but never the name,
    /// so a drop in a namespace a split would have rooted *at* may be that very
    /// entity — and an absence proof that ignores it is the standing
    /// absent-versus-unread confusion
    /// ([`Self::dropped_type_could_root_this_path`] is the type path's gate for
    /// the same hazard). Real inputs make this nearly free: drops are rare
    /// enough that the whole-project differential does not move.
    ///
    /// The drop is asked of **exactly the namespaces the rooting loop visits**,
    /// not of every split of the path. A drop recorded in `names` entire is a
    /// type *inside* the whole path — a child of the leaf, which no rooting
    /// could be — and one shallower than `base` sits inside the reading prefix,
    /// which this walk never roots at either. Widening past the loop's own range
    /// would decline `List.rev` inside `namespace N; module List` on a drop in
    /// `N.List.rev`, losing the fall-through to FSharp.Core for a type that
    /// cannot occupy the terminal segment (codex review round 2).
    fn any_rooting_position(&self, names: &[String], base: usize) -> bool {
        (base..names.len()).any(|k| {
            !self.rooting_candidates(&names[..k], &names[k]).is_empty()
                || self.assemblies.namespace_has_dropped_type(&names[..k])
        })
    }

    /// The one owner a reading may commit, or `None` when the owners are
    /// contested.
    ///
    /// `owners` is in walk order — longest position first, and within a position
    /// [`Self::rooting_candidates`]'s order — so its head is the
    /// highest-priority one. A single owner commits. Several commit only in the
    /// measured tie-break: an **authoritative module** ahead of owners that are
    /// all its own same-position, same-DLL companions, which is `Holder.Tail`
    /// binding the module's `let` over the type's `static member` (fsi).
    /// Anything else — two arities of one type family, two DLLs, two positions —
    /// is a choice we cannot make the way FCS makes it, so it defers.
    fn unopposed_owner(&self, owners: &[PositionOwner]) -> Option<Vec<(TextRange, Resolution)>> {
        let (first, rest) = owners.split_first()?;
        if rest.is_empty() {
            return Some(first.payload.clone());
        }
        let companions = self.assemblies.is_authoritative_module(first.handle)
            && rest.iter().all(|o| {
                o.position == first.position
                    && self.assemblies.distinct_dlls(&[first.handle, o.handle]) == 1
            });
        companions.then(|| first.payload.clone())
    }

    /// The reading at one rooting **position**: every candidate that owns the
    /// path, plus the highest-priority partial for when none does.
    ///
    /// Every candidate is walked — there is no early return on the first owner —
    /// because the caller's commit rule is *uniqueness*, and it cannot see a
    /// second owner this function never looked for. `Err` carries a verdict that
    /// decides the whole reading where it sits (a merged rooting, an opaque
    /// abbreviation, a project shadow).
    fn rooting_at(
        &self,
        names: &[String],
        segments: &[SyntaxToken],
        base: usize,
        k: usize,
        candidates: &[EntityHandle],
    ) -> Result<PositionReading, AssemblyPath<Vec<(TextRange, Resolution)>>> {
        let mut reading = PositionReading {
            owners: Vec::new(),
            partial: None,
        };
        for &candidate in candidates {
            match self.rooting_reading(names, segments, base, k, candidate) {
                // A reading that stops on a non-value owns nothing: FCS's
                // expression-position lookup wants a value, and a module — or a
                // record, union, interface, enum — is not one. Demoting it to a
                // partial here, rather than letting the candidate order decide,
                // is what keeps such an entity from capturing a path that a
                // shorter rooting or another candidate really supplies.
                AssemblyPath::Resolved { payload, .. }
                    if self.reading_stops_on_a_non_value(segments, &payload) =>
                {
                    if reading.partial.is_none() {
                        reading.partial = Some(payload);
                    }
                }
                AssemblyPath::Resolved {
                    payload,
                    owns_path: true,
                } => reading.owners.push(PositionOwner {
                    position: k,
                    handle: candidate,
                    payload,
                }),
                AssemblyPath::Resolved {
                    payload,
                    owns_path: false,
                } => {
                    if reading.partial.is_none() {
                        reading.partial = Some(payload);
                    }
                }
                AssemblyPath::NoMatch => {}
                // An undecidable candidate — an opaque abbreviation, a merged
                // rooting — stays a **contender**: it might be what FCS binds,
                // so a candidate the walk reached earlier must not commit over
                // it. The single exception is the proven companion tie, an
                // authoritative module ahead of same-DLL siblings, which is
                // `WidgetC.Make`: a `ModuleSuffix` module beside an abbreviation
                // of the same name, where FCS binds the module's `Make`
                // (fcs-dump) and the alias must not veto it (codex review).
                verdict => {
                    let companion_tie = reading.owners.first().is_some_and(|owner| {
                        self.assemblies.is_authoritative_module(owner.handle)
                            && self.assemblies.distinct_dlls(&[owner.handle, candidate]) == 1
                    });
                    if !companion_tie {
                        return Err(verdict);
                    }
                }
            }
        }
        Ok(reading)
    }

    /// The public top-level entities one reading may root at, in the order F#
    /// prefers them when more than one **owns** the path.
    ///
    /// One `(namespace, name)` holds a whole set: a type and its companion
    /// module (`type TypeInfo<'a,'b>` beside `module TypeInfo`, which fsc emits
    /// as `TypeInfoModule` while F# source spells it bare), and types at
    /// several generic arities. The order is **module first, then types by
    /// ascending arity**:
    ///
    /// - module over type is fsi-measured — with `let Tail` in the module and a
    ///   `static member Tail` on the type, `Holder.Tail` returns the module's.
    ///   **Authoritative** module-ness only: a non-authoritative assembly's
    ///   `Module` kind is an IL heuristic FCS does not share (it imports the
    ///   type through IL, where a module reads as a plain type), so preferring
    ///   it there would move the candidate FCS actually binds out of first place
    ///   (codex review). Such an entity keeps its place among the types;
    /// - the *written* arity of a value-path head is always 0 (a type
    ///   application cannot appear there), and FCS infers the type arguments
    ///   rather than requiring them (`FS1125`), so a generic type is reachable
    ///   here at all — which is why the arity-keyed first-wins slot this
    ///   replaced could not see `MethodBody<'a>` under a bare `MethodBody`.
    fn rooting_candidates(&self, namespace: &[String], name: &str) -> Vec<EntityHandle> {
        let mut candidates = self.assemblies.public_entities_named(namespace, name);
        candidates.sort_by_key(|&h| {
            (
                !self.assemblies.is_authoritative_module(h),
                self.assemblies.entity(h).generic_parameters.len(),
            )
        });
        candidates
    }

    /// Whether a reading's **terminal segment** landed on an entity that is not
    /// an expression value — the shape that captures no path.
    ///
    /// FCS's expression-position lookup wants a value: `Lib.C` binds the class
    /// `C`'s constructor, and where `C` is a module, a record, a union, an
    /// interface, an enum or a static class it binds *nothing at all* and the
    /// lookup goes on elsewhere (fcs-dump-measured per shape). So such a reading
    /// is a **non-owner** rather than merely a lower preference — the candidate
    /// order must not decide it, since with no tail to walk every candidate
    /// would otherwise own the path and the order alone would pick, and a
    /// *shorter* rooting whose module supplies a `let` of the name must still
    /// win (codex review rounds 2 and 3). The type walk has the same rule for a
    /// module leaf.
    ///
    /// A resolved static member or union case records across the *whole* path
    /// rather than the terminal segment, so this recognises only a bare entity.
    /// A **non-authoritative** module is deliberately not treated as one: there
    /// its `Module` kind is an IL heuristic FCS does not share, and it imports
    /// the type as a plain class, so [`AssemblyEnv::terminal_expression_value`](crate::AssemblyEnv::terminal_expression_value)
    /// answers for the shape FCS actually sees.
    fn reading_stops_on_a_non_value(
        &self,
        segments: &[SyntaxToken],
        payload: &[(TextRange, Resolution)],
    ) -> bool {
        segments.last().is_some_and(|last| {
            let terminal = last.text_range();
            payload.iter().any(|&(range, res)| {
                range == terminal
                    && matches!(res, Resolution::Entity(h) if !self.assemblies.terminal_expression_value(h))
            })
        })
    }

    /// One rooting candidate's reading: the cross-DLL merge check at *its* key,
    /// then the descent below it.
    ///
    /// FCS merges same-FQN roots across references and binds the latest
    /// accessible one, which sema does not model, so at a merged rooting we can
    /// never name a target. The merge is nonetheless *tail-sensitive*: when no
    /// contestant supplies the tail FCS skips the rooting entirely, so the
    /// collision is decided by counting **suppliers**
    /// ([`Self::contest_has_a_supplier`]), not by the collision itself.
    ///
    /// The count is per `(namespace, name, arity)` — the key a merge actually
    /// happens at — so a same-DLL companion module, and a `type Alias = Widget`
    /// beside a generic `type Alias<'T>`, are not collisions (codex review).
    fn rooting_reading(
        &self,
        names: &[String],
        segments: &[SyntaxToken],
        base: usize,
        k: usize,
        candidate: EntityHandle,
    ) -> AssemblyPath<Vec<(TextRange, Resolution)>> {
        let arity = self.assemblies.entity(candidate).generic_parameters.len();
        let contestants =
            self.assemblies
                .public_types_named_at_arity(&names[..k], &names[k], arity);
        if self.assemblies.distinct_dlls(&contestants) > 1 {
            let supplied = self.contest_has_a_supplier(
                contestants,
                |handle| self.assembly_path_records_from_root(names, segments, base, k, handle),
                // A reading that stops on a non-value supplies nothing — see
                // [`Self::reading_stops_on_a_non_value`]. This is not commit safety
                // (nothing is committed at a contest); it decides whether the
                // contest has *any* supplier, and so whether a lower-priority
                // reading that does resolve may still win (codex review round
                // 8).
                |reading| {
                    reading.may_own_path()
                        && !matches!(
                            reading,
                            AssemblyPath::Resolved { payload, .. }
                                if self.reading_stops_on_a_non_value(segments, payload)
                        )
                },
            );
            return if supplied {
                AssemblyPath::ContestedRooting
            } else {
                // No candidate supplies the tail: an ordinary **partial**
                // reading a lower priority may supersede. Its records defer at
                // every segment — the rooting exists, but naming *which* DLL's
                // type it is remains the thing we cannot do.
                AssemblyPath::Resolved {
                    payload: segments
                        .iter()
                        .map(|seg| {
                            (
                                seg.text_range(),
                                Resolution::Deferred(DeferredReason::QualifiedAccess),
                            )
                        })
                        .collect(),
                    owns_path: false,
                }
            };
        }

        self.assembly_path_records_from_root(names, segments, base, k, candidate)
    }

    /// Whether **any** candidate at a contested rooting FQN could satisfy the
    /// path — the one place both walks decide a cross-DLL collision.
    ///
    /// FCS merges same-FQN roots across differently-named references and binds
    /// the latest *accessible* one, which sema does not model (`lookup_type` is
    /// first-wins), so at a merged rooting we can never name a target: any
    /// supplier at all means **defer**. The merged lookup is nonetheless
    /// *tail-sensitive* — when no candidate supplies the tail FCS skips the
    /// rooting entirely (fsi-verified 2026-07-25 with three probe libraries) —
    /// so "any supplier" is the question worth asking, and a `false` here lets
    /// a lower-priority reading that completes the path win.
    ///
    /// Committing when exactly *one* candidate supplies the path would match
    /// FCS more often (fsi: with only `DupA`'s `High.Color` carrying `OnlyOnA`,
    /// `Color.OnlyOnA` binds it despite being the earlier reference), and an
    /// earlier revision did. It is deliberately not done: "supplies the path"
    /// has to agree with FCS's notion exactly for such a commit to be safe, and
    /// review found three shapes where it did not — a terminal module, a
    /// non-authoritative module kind, and a union-case tail (union constructors
    /// live in `union_cases`, not `members`, so an owning union reads as
    /// absent). Each was a *wrong target*, the one outcome D5 forbids, whereas
    /// deferring costs only coverage in an already-rare shape.
    ///
    /// The caller supplies `walk`, its own post-rooting descent, and
    /// `is_supplier`. `may_own_path` is the base test: everything except a
    /// genuinely-absent tail, so a candidate that *defers* still counts (it may
    /// satisfy the path on a surface we do not model).
    /// `is_supplier` is the position's own test, since "can satisfy the path"
    /// is not the same question in both walks: the type walk additionally
    /// rejects a **module leaf**, which is outside the terminal type namespace
    /// at every depth (codex review round 5).
    fn contest_has_a_supplier<T>(
        &self,
        candidates: Vec<EntityHandle>,
        walk: impl Fn(EntityHandle) -> AssemblyPath<T>,
        is_supplier: impl Fn(&AssemblyPath<T>) -> bool,
    ) -> bool {
        candidates
            .into_iter()
            .any(|handle| is_supplier(&walk(handle)))
    }

    /// One reading's walk *below* an already-chosen rooting type — the body of
    /// [`Self::assembly_path_records`] past its longest-public-type-prefix
    /// search, parameterised by the rooting `type_handle` so a **contested**
    /// rooting can ask the same question of each contestant rather than of the
    /// first-wins slot alone.
    ///
    /// `names` is the full prefix-plus-segments path, `base` the prefix length
    /// and `k` the index of the rooting segment (`names[k]`, source token
    /// `segments[k - base]`). Never returns [`AssemblyPath::NoMatch`]: the
    /// rooting is given, so the reading at minimum matches partially.
    fn assembly_path_records_from_root(
        &self,
        names: &[String],
        segments: &[SyntaxToken],
        base: usize,
        k: usize,
        type_handle: EntityHandle,
    ) -> AssemblyPath<Vec<(TextRange, Resolution)>> {
        let n = names.len();

        // A type-abbreviation marker: the name binds, and FCS chases the
        // abbreviation to its target (`S.Format` where `type S = System.String`
        // resolves `Format` on `System.String`). Resolve *through* a resolvable
        // target — the tail then walks on the target below — but keep the marker
        // itself as the binding for the alias segment. An unresolvable target
        // (structural, generic, or not loaded) shadow-defers as before (D5: defer,
        // never a wrong target).
        let walk_root = if self.assemblies.is_abbreviation(type_handle) {
            // Resolve-through is unsafe when the alias has a ModuleSuffix
            // companion module, whose member FCS routes `Alias.Member` to, not
            // the target's (fcs-verified) — a module-over-target precedence we
            // do not model (codex review). Defer, as the marker did before
            // Stage 4.
            if self
                .assemblies
                .alias_has_companion_module(type_handle, None)
            {
                return AssemblyPath::Occupied(DeclineCause::AliasCompanionModule);
            }
            match self.assemblies.resolve_abbreviation_target(type_handle) {
                Some(target) => target,
                None => return AssemblyPath::Occupied(DeclineCause::AliasTargetUnchaseable),
            }
        } else {
            type_handle
        };
        // True once the path roots (or descends) through a *resolved* abbreviation
        // alias. FCS then owns the alias reading, so an `Absent` member tail must
        // NOT cede the path to a lower reading: the tail may live on a non-member
        // target surface — a union case (`union_cases`) or a type
        // augmentation — that we do not walk, so absence from `members` does not
        // prove absence (codex review 4 on Stage 4).
        let mut via_alias = walk_root != type_handle;

        let mut recs: Vec<(TextRange, Resolution)> = Vec::new();
        let deferred = Resolution::Deferred(DeferredReason::QualifiedAccess);
        // Namespace qualifier segments that are in the *source* (indices
        // `base..k`) are modeled uses we cannot resolve — defer, never drop.
        for seg in &segments[..(k - base)] {
            recs.push((seg.text_range(), deferred));
        }
        recs.push((
            segments[k - base].text_range(),
            // Resolve-through chases the target to walk a member *tail*. When the
            // alias is a *qualifier* (a tail follows: `WidgetAlias.Make`), FCS points
            // the alias segment at the marker and the tail below walks on `walk_root`
            // (its target) — bind the marker. But a *bare* alias use with no tail
            // (`Lib.WidgetAlias`) FCS resolves by the target's value/constructor
            // surface, which we do not model: a constructible class points at the
            // terminal type, a `type UAlias = U` union without a constructor errors
            // FS1133 with no symbol at all (codex review). We cannot tell those apart
            // here, so *defer* the bare alias — own the path (it is the alias's own
            // reading) but name no target, never a wrong one. (A *type*-position bare
            // use is a separate resolver, [`Self::assembly_type_path_core`].)
            if via_alias && k + 1 == n {
                deferred
            } else {
                Resolution::Entity(type_handle)
            },
        ));

        // Walk the segments past the rooting type: nested types extend the
        // chain; a public *static* member ends it (a type-qualified path
        // resolves only static members; FCS reports the member spanning the
        // whole path). `owns_path` records whether the reading captures the whole
        // path — see [`AssemblyPath::Resolved`]; it stays `true` unless a segment
        // names nothing on its parent (a genuinely-absent tail).
        let mut parent = walk_root;
        let mut i = k + 1;
        let mut owns_path = true;
        while i < n {
            let src = &segments[i - base];
            // A union **case** ending the path is a case reference, not a
            // nested-type descent: FCS reports it over the whole long
            // identifier, and a field-carrying case's nested carrier — which the
            // descent below would otherwise take, recording at the segment — is
            // that same case's compiled form. Terminal-only, so a deeper path
            // through the carrier (`U.Case.Item`) still descends.
            if i + 1 == n
                && let Some(case) = self.union_case_tail(parent, &names[i])
            {
                let whole =
                    TextRange::new(segments[0].text_range().start(), src.text_range().end());
                recs.push((whole, case));
                i += 1;
                break;
            }
            if let Some(child) = self
                .assemblies
                .nested(parent, &names[i], 0)
                .filter(|&h| self.assemblies.is_public(h))
            {
                // A nested abbreviation marker: resolve through its target (or
                // shadow-defer if unresolvable), same as the rooting case above.
                let child_walk = if self.assemblies.is_abbreviation(child) {
                    // Defer for the same reason as the rooting branch: a
                    // companion module beside this nested alias (codex review).
                    // (A merged parent — the rooting FQN colliding across DLLs
                    // — already deferred the whole path at the rooting above.)
                    if self
                        .assemblies
                        .alias_has_companion_module(child, Some(parent))
                    {
                        return AssemblyPath::Occupied(DeclineCause::AliasCompanionModule);
                    }
                    match self.assemblies.resolve_abbreviation_target(child) {
                        Some(target) => {
                            // A *terminal* nested alias (no tail follows) is a bare
                            // use — defer it exactly as the rooting branch does, for
                            // the same reason (we do not model the target's
                            // value/constructor surface; codex review). Only a
                            // *qualifier* nested alias resolves through to a tail.
                            if i + 1 == n {
                                recs.push((src.text_range(), deferred));
                                i += 1;
                                break;
                            }
                            via_alias = true;
                            target
                        }
                        None => {
                            return AssemblyPath::Occupied(DeclineCause::AliasTargetUnchaseable);
                        }
                    }
                } else {
                    child
                };
                recs.push((src.text_range(), Resolution::Entity(child)));
                parent = child_walk;
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
                        // …unless the occupying entity is a *generic
                        // type-abbreviation* child the arity-0 `nested` step above
                        // missed (a val would have resolved above; only a
                        // non-value occupant reaches here). Its target is
                        // unmodelled and FCS's ownership is target-sensitive — a
                        // record/union target falls through, a class target keeps
                        // the module — so we can commit neither: defer the whole
                        // path (codex review 3). `AbbreviationOpaque` defers
                        // *tier-locally* (unlike `Occupied` it does not
                        // preempt a higher-priority open that resolves the path —
                        // codex review 4).
                        if self
                            .assemblies
                            .has_public_abbreviation_child(parent, &names[i])
                        {
                            return AssemblyPath::AbbreviationOpaque;
                        }
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
                        // Through a resolved alias FCS owns this reading, and the
                        // tail may live on a non-member target surface we do not
                        // walk (see `via_alias`). Own-and-defer as the
                        // pre-resolve-through marker did, rather than cede the path
                        // to a lower reading and diverge from FCS.
                        if via_alias {
                            return AssemblyPath::Occupied(DeclineCause::AliasOwnedTail);
                        }
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

    /// How a path segment that names a **union case** of `parent` resolves, or
    /// `None` when `parent` is not a union that declares it.
    ///
    /// The case must be *provable* — [`AssemblyEnv::authoritative_union_case`](crate::AssemblyEnv::authoritative_union_case),
    /// so neither an unknowable case list nor a non-authoritative assembly's
    /// IL-heuristic union can make a reading own a path FCS would re-root.
    ///
    /// A field-carrying case compiles to a nested type — nameable, and keyed on
    /// the union's own arity, whose generic parameters the carrier inherits. A
    /// **nullary** case has no carrier at all (fsc emits a static property the
    /// F#-entity projection drops), so it defers: the reading owns the path (the
    /// case is certainly there) while naming no target.
    ///
    /// [`AssemblyEnv::authoritative_union_case`](crate::AssemblyEnv::authoritative_union_case): crate::AssemblyEnv::authoritative_union_case
    fn union_case_tail(&self, parent: EntityHandle, name: &str) -> Option<Resolution> {
        if !self.assemblies.authoritative_union_case(parent, name) {
            return None;
        }
        let arity = self.assemblies.entity(parent).generic_parameters.len();
        Some(
            match self
                .assemblies
                .nested(parent, name, arity)
                .or_else(|| self.assemblies.nested(parent, name, 0))
                .filter(|&h| self.assemblies.is_public(h))
            {
                Some(case) => Resolution::Entity(case),
                None => Resolution::Deferred(DeferredReason::QualifiedAccess),
            },
        )
    }

    /// The **union-case pattern** sibling of [`Self::assembly_type_path_core`]:
    /// decide — without recording — how a bare `Type.Case` *pattern* reads under
    /// one namespace `prefix`. A pattern head is a lookup in F#'s *constructor*
    /// namespace, not the value one, which is what makes this a third leaf rather
    /// than a reuse of either sibling:
    ///
    /// - a **module** named `type_name` is transparent (a module is not a type),
    ///   so a prefix holding only modules is [`AssemblyPath::NoMatch`] and the
    ///   walk continues outward — this is what roots `Fantomas.FCS.Syntax.SynType`
    ///   past a later-`open`ed `WoofWare.Whippet.Fantomas.SynType` *module*;
    /// - the reading **owns its prefix** ([`AssemblyPath::Resolved`]'s `owns_path`)
    ///   as soon as a union here declares the case, keyed on
    ///   [`union_cases`](borzoi_assembly::Entity::union_cases) rather
    ///   than on a nested type existing. A **nullary** case compiles to a
    ///   singleton with no nested IL type, so keying ownership on the nested walk
    ///   (as [`Self::assembly_type_path_core`] must) would demote it to a
    ///   fallback a lower prefix could beat — a wrong target, not a missed one.
    ///
    /// Anything else named `type_name` here — a non-union type, an abbreviation,
    /// a union with an unknowable case list or one lacking this case, or a second
    /// union owning it (distinct arities; the pattern writes none) — makes the
    /// reading [`AssemblyPath::Occupied`]
    /// ([`CasePatternHeadOccupied`](DeclineCause::CasePatternHeadOccupied)): the
    /// walk decides at this prefix and never falls through past it. A
    /// **cross-DLL** collision defers the same way but is a different occupant
    /// ([`ContestedRooting`](DeclineCause::ContestedRooting) — FCS merges
    /// same-FQN roots by reference order, which sema does not model); a
    /// same-DLL companion — a `type Shape` beside its `module Shape` — is not a
    /// collision.
    ///
    /// That is a **conservative** decline, not F#'s rule. F# keeps searching
    /// outward for something that declares the case, so a class or a caseless
    /// union of the name is in fact transparent too (FCS-pinned by
    /// `resolve_case_pattern_gen_diff.rs`, which resolves the union past both).
    /// Telling "provably cannot supply this name" from "occupies it" needs the
    /// entity's whole member/nested/literal surface, which not every projected
    /// entity has; until it does, declining costs availability at those prefixes
    /// and never a wrong target.
    pub(super) fn assembly_case_pattern_records(
        &self,
        prefix: &[String],
        type_name: &str,
        case_name: &str,
        type_seg: &SyntaxToken,
        case_seg: &SyntaxToken,
    ) -> AssemblyPath<Vec<(TextRange, Resolution)>> {
        let here = self.assemblies.public_entities_named(prefix, type_name);
        if here.is_empty() {
            return AssemblyPath::NoMatch;
        }
        // Distinct loaded-DLL *provenance* — see the doc comment. Not the
        // manifest identity: two loaded DLLs can share one, and merging them
        // here would hide a module-vs-union collision FCS resolves by reference
        // order. [`AssemblyEnv::distinct_dlls`] is the one rule for that, shared
        // with the rooting-collision counts, so the two cannot drift apart into
        // disagreeing about what a collision is.
        if self.assemblies.distinct_dlls(&here) > 1 {
            return AssemblyPath::Occupied(DeclineCause::ContestedRooting);
        }
        let mut owning: Option<EntityHandle> = None;
        let mut binds = false;
        for &h in &here {
            let entity = self.assemblies.entity(h);
            if self.assemblies.authoritative_union_case(h, case_name) {
                binds |= owning.replace(h).is_some();
            } else if entity.kind != EntityKind::Module {
                binds = true;
            }
        }
        if binds {
            return AssemblyPath::Occupied(DeclineCause::CasePatternHeadOccupied);
        }
        // Only plain modules here: a transparent prefix the walk reads past.
        let Some(union) = owning else {
            return AssemblyPath::NoMatch;
        };
        // A field-carrying case compiles to a nested type; a generic union's
        // carrier carries the union's own generic parameters, so key the nested
        // lookup on the union's arity (falling back to arity 0 for a non-generic
        // union). A nullary case has no carrier at all — a known case reference
        // with an opaque target, exactly as an opened assembly case folds.
        let arity = self.assemblies.entity(union).generic_parameters.len();
        let nested_case = self
            .assemblies
            .nested(union, case_name, arity)
            .or_else(|| self.assemblies.nested(union, case_name, 0))
            .filter(|&h| self.assemblies.is_public(h));
        let whole = TextRange::new(type_seg.text_range().start(), case_seg.text_range().end());
        let recs = vec![
            (type_seg.text_range(), Resolution::Entity(union)),
            (
                whole,
                match nested_case {
                    Some(case) => Resolution::Entity(case),
                    None => Resolution::Deferred(DeferredReason::QualifiedAccess),
                },
            ),
        ];
        AssemblyPath::Resolved {
            payload: recs,
            owns_path: true,
        }
    }

    /// The open readings contributed by **explicit source `open`s** only —
    /// the top of the precedence ladder. Opens are yielded **latest-first**
    /// (F# is latest-open-wins, not ambiguity), and within one open its readings
    /// as the group orders them (relative before merged root — see
    /// [`OpenGroup`](super::state::OpenGroup)). Explicit opens are appended after
    /// the implicit seed, so the latest-first iteration yields them first;
    /// `implicit_import_count` marks the split, and it is the same boundary
    /// [`Self::implicit_open_reading_prefixes`] cuts at.
    ///
    /// This is also the stratum that outranks a module-shaped manifest
    /// auto-open's surface: that surface is opened at file start, so every
    /// explicit open is later and wins — see
    /// [`Self::prefixes_outranking_the_manifest_surface`] for the whole stratum.
    pub(super) fn explicit_open_reading_prefixes(
        &self,
    ) -> impl Iterator<Item = (DeclineTier, &[String])> {
        self.imports[self.implicit_import_count..]
            .iter()
            .rev()
            .flat_map(|open| {
                open.readings
                    .iter()
                    .map(|r| (DeclineTier::ExplicitOpen, r.as_slice()))
            })
    }

    /// The open readings contributed by the **implicit** opens — `FSharp.Core`'s
    /// seed and each namespace-shaped `[<assembly: AutoOpen>]` — which sit
    /// *below* the enclosing namespace, not above it.
    ///
    /// That boundary is the compiler's, not a probe's inference:
    /// `CheckDeclarations.fs` builds the initial environment by folding
    /// `AddCcuToTcEnv` over the referenced assemblies (each one's root contents,
    /// then its manifest `[<assembly: AutoOpen>]`s), and only afterwards, on
    /// entering the file's namespace declaration group, runs
    /// `ImplicitlyOpenOwnNamespace` — "Inside `namespace X.Y.Z` there is an
    /// implicit open of `X.Y.Z`". FCS's name environment is last-write-wins, so
    /// the later write outranks: enclosing namespace over every implicit open.
    /// The file's own `open`s are later still.
    ///
    /// Ordering *within* this stratum is reference order, which we do not model
    /// — see the `TNsRo`/`DNsRo` rows of `tier_order_diff`'s `KNOWN_DIVERGENCES`
    /// for what that still costs at the boundary with the root tier.
    pub(super) fn implicit_open_reading_prefixes(
        &self,
    ) -> impl Iterator<Item = (DeclineTier, &[String])> {
        self.imports[..self.implicit_import_count]
            .iter()
            .rev()
            .flat_map(|open| {
                open.readings
                    .iter()
                    .map(|r| (DeclineTier::ImplicitOpen, r.as_slice()))
            })
    }

    /// Every reading that out-ranks a **module-shaped manifest auto-open's**
    /// imported surface, in priority order — what
    /// [`Resolver::decide_type_path`]'s manifest veto walks before deferring.
    ///
    /// The surface sits at open priority but below both tiers above it:
    ///
    /// ```text
    /// explicit opens  >  enclosing namespace  >  manifest surface  >  root
    /// ```
    ///
    /// so this is exactly the leading two tiers of
    /// [`Self::assembly_prefixes_by_priority`] — everything above the implicit
    /// opens, of which the manifest surface is one. Being a genuine *prefix* of
    /// that sequence is what [`Self::resolve_assembly_path_over`] requires of
    /// the prefixes it is handed, so the two cannot disagree about tier order.
    /// All three boundaries are fsi-verified against the
    /// `autoopen_env` fixture's decoys — a `namespace global` type for the
    /// root boundary, `open SemaAutoOpen.ExplicitBeats` for the explicit-open
    /// one, and `namespace SemaAutoOpen.ExplicitBeats` for the
    /// enclosing-namespace one.
    pub(super) fn prefixes_outranking_the_manifest_surface(
        &self,
    ) -> impl Iterator<Item = (DeclineTier, &[String])> {
        self.explicit_open_reading_prefixes().chain(
            Some(self.enclosing_namespace())
                .filter(|e| !e.is_empty())
                .map(|e| (DeclineTier::EnclosingNamespace, e)),
        )
    }

    /// Every prefix a dotted path may be read under, in strict F# precedence
    /// order — the readings [`Self::resolve_assembly_path_tiered`] walks:
    /// 1. **explicit source `open`s** ([`Self::explicit_open_reading_prefixes`]);
    /// 2. the **current enclosing namespace** ([`Self::enclosing_namespace`]):
    ///    FS0039 — the current namespace's child, never an ancestor, never a
    ///    module segment past it;
    /// 3. the **implicit opens** ([`Self::implicit_open_reading_prefixes`]);
    /// 4. **root / as-written** (the empty prefix).
    ///
    /// The enclosing namespace sits *between* the two open strata because that
    /// is the order the compiler writes them in — the reasoning is on
    /// [`Self::implicit_open_reading_prefixes`], which is where a reader looking
    /// for "why is FSharp.Core below my own namespace?" will land.
    ///
    /// `pub(super)` so the unmodelled-open guard in `lookup.rs` iterates the
    /// same sequence — a tier added here must be visible to that guard too.
    pub(super) fn assembly_prefixes_by_priority(
        &self,
    ) -> impl Iterator<Item = (DeclineTier, &[String])> {
        const ROOT: &[String] = &[];
        self.explicit_open_reading_prefixes()
            .chain(
                Some(self.enclosing_namespace())
                    .filter(|e| !e.is_empty())
                    .map(|e| (DeclineTier::EnclosingNamespace, e)),
            )
            .chain(self.implicit_open_reading_prefixes())
            .chain(std::iter::once((DeclineTier::Root, ROOT)))
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
    /// [`ShadowVeto::Vetoed`] verdict (exact metadata) vetoes even a
    /// same-tier real match — FCS-probed: `namespace Ns; type Foo = …;
    /// [<AutoOpen>] module Auto = type Foo = …` then `open Ns; (x : Foo)`
    /// binds `Ns.Auto.Foo`, not the direct `Ns.Foo` (found by review, round
    /// 6, on `docs/completed/r2-annotation-typing-plan.md`). The value path passes
    /// `|_| ShadowVeto::None`: sema already enumerates an auto-open module's
    /// *values* (only its nested *types* are unmodelled), and it has no
    /// coarse unmodelled-shadow source once past the
    /// `unmodelled_open_active` guard its caller applies first. It is asked at
    /// **every** tier the walk visits, a held partial fallback
    /// notwithstanding — see the fallback's own comment for why a partial
    /// cannot switch the lower tiers' verdicts off.
    pub(super) fn resolve_assembly_path_tiered<R>(
        &self,
        records: impl Fn(&[String]) -> AssemblyPath<R>,
        as_written_vetoes_opens: bool,
        shadow_at: impl Fn(&[String]) -> ShadowVeto,
    ) -> TieredResolution<R> {
        self.resolve_assembly_path_over(
            self.assembly_prefixes_by_priority(),
            records,
            as_written_vetoes_opens,
            shadow_at,
        )
    }

    /// [`Self::resolve_assembly_path_tiered`]'s walk over an **explicit
    /// prefix sequence** instead of the full
    /// [`Self::assembly_prefixes_by_priority`] — for a caller that must stop
    /// the walk at a priority boundary an unmodelled surface sits below
    /// ([`Resolver::decide_type_path`]'s manifest veto walks only
    /// [`Self::explicit_open_reading_prefixes`]: a complete reading there
    /// outranks the manifest module surface; anything lower may not).
    /// `prefixes` must be a prefix of the full priority sequence, or the
    /// tier ordering the verdicts assume does not hold.
    pub(super) fn resolve_assembly_path_over<'p, R>(
        &self,
        prefixes: impl Iterator<Item = (DeclineTier, &'p [String])>,
        records: impl Fn(&[String]) -> AssemblyPath<R>,
        as_written_vetoes_opens: bool,
        shadow_at: impl Fn(&[String]) -> ShadowVeto,
    ) -> TieredResolution<R> {
        // The veto's root reading is held and consumed when the walk reaches the
        // ROOT tier (the final, empty prefix), instead of recomputing it —
        // `records` is pure, and the root reading is the common case's most
        // expensive one to duplicate.
        let mut root = as_written_vetoes_opens.then(|| records(&[]));
        if let Some(AssemblyPath::Occupied(cause)) = root {
            return TieredResolution::ShadowDeferred(DeclineSite {
                cause,
                tier: DeclineTier::Root,
            });
        }

        // The highest-priority partial reading seen so far; the result only if
        // the whole walk ends with no owning reading and no project shadow. A
        // reading is *owning* (`owns_path`) iff it captures the whole path — a
        // nested-type chain, a unique static member, or an overload set the type
        // owns but cannot uniquely select (see [`AssemblyPath::Resolved`]).
        //
        // Holding one does **not** switch the shadow verdict off for the tiers
        // below it. A partial does not own the path, and the loop below hands a
        // *lower* owning reading the win over it — so a risk between the two is
        // a reading that may own the path above whatever eventually does, and
        // skipping it commits under it. It invalidates the held partial as well,
        // for the same reason: what the risk hides may own the whole path where
        // the partial only reached its rooting.
        let mut fallback: Option<(R, DeclineTier)> = None;

        for (tier, prefix) in prefixes {
            if let ShadowVeto::Vetoed(cause) = shadow_at(prefix) {
                return TieredResolution::ShadowDeferred(DeclineSite { cause, tier });
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
            // Read before the match consumes the reading; `None` for the two
            // variants that are not declines.
            let declining = reading.decline_cause();
            match reading {
                AssemblyPath::Resolved { payload, owns_path } => {
                    if owns_path {
                        return TieredResolution::Resolved { payload, tier };
                    }
                    fallback.get_or_insert((payload, tier));
                }
                // A generic-abbreviation reading defers, like a project shadow —
                // but only once *reached in priority order*. The preemptive
                // as-written-root check above skips it (it is not
                // `Occupied`), so a higher-priority `open` resolving the
                // path is tried first and wins; the abbreviation defers only if
                // it is the highest reading left (codex review 4). A
                // self-module-shadowed root reading is likewise skipped by the
                // preemptive check and defers here — after every open has
                // declined — so the current module's own name still shadows the
                // same-named *root* namespace's assembly reading.
                AssemblyPath::Occupied(_)
                | AssemblyPath::SelfModuleShadowed
                | AssemblyPath::AbbreviationOpaque
                | AssemblyPath::ContestedRooting => {
                    let cause = declining.expect("a declining reading names its cause");
                    return TieredResolution::ShadowDeferred(DeclineSite { cause, tier });
                }
                AssemblyPath::NoMatch => {}
            }
        }
        match fallback {
            Some((payload, tier)) => TieredResolution::Resolved { payload, tier },
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
        // shadowing that the expression sibling's `project_shadow_cause` adds.
        if let Some(cause) = self.project_type_shadow_cause(&names) {
            return AssemblyPath::Occupied(cause);
        }

        // Longest prefix `[..k]` (with `k >= base`, a source segment) whose
        // `(namespace, name)` is a public top-level type. The arity is applied to
        // the final segment only — an encloser in the path is keyed at arity 0.
        let arity_at = |k: usize| if k == n - 1 { arity } else { 0 };
        // An F# **module** never occupies a *terminal* type position: FCS
        // imports it as a `ModuleOrNamespace`, outside the type namespace
        // entirely, so `let y : Ns.Color` with only a `module Ns.Color` loaded
        // is FS0039 (fsi-verified 2026-07-25) — committing the module there
        // would be a wrong target. As a *container* for a nested-type tail it
        // is perfectly good (`Ns.Color.Inner` resolves), so the filter is
        // position-scoped, not kind-blanket.
        let type_position_candidates = |k: usize| {
            let mut candidates =
                self.assemblies
                    .public_types_named_at_arity(&names[..k], &names[k], arity_at(k));
            if k + 1 == n {
                candidates.retain(|&h| !self.assemblies.is_authoritative_module(h));
            }
            candidates
        };
        // The first-wins slot stays the selector wherever it is eligible — its
        // tie-breaking (a source-named type outranking a suffixed companion
        // module) is load-order-independent and already FCS-pinned. When the
        // slot holds an ineligible candidate the walk prefers the first
        // eligible one in the bucket, so a same-FQN class in another DLL is
        // still found behind a module that happened to index first.
        //
        // With *no* eligible candidate the slot is kept as-is: a lone module in
        // terminal type position still roots the reading, and the leaf-kind
        // check downstream in `Resolver::decide_type_path` declines it off
        // [`TypePathReading::leaf`]. The two layers divide the work — this
        // filter settles which candidates *contest*, the leaf-kind check
        // settles whether the winner may be *committed* — so a module that is
        // the only thing at an FQN needs no special case here.
        let Some((k, candidates, type_handle)) = (base..n).rev().find_map(|k| {
            let candidates = type_position_candidates(k);
            let slot = self
                .assemblies
                .lookup_type(&names[..k], &names[k], arity_at(k))
                .filter(|&handle| self.assemblies.is_public(handle));
            let selected = match slot {
                Some(handle) if candidates.contains(&handle) => handle,
                other => candidates.first().copied().or(other)?,
            };
            Some((k, candidates, selected))
        }) else {
            return AssemblyPath::NoMatch;
        };

        // The type-path mirror of the value/member walk's contest (see
        // [`Self::contest_has_a_supplier`]). It counts distinct DLLs over the
        // **type-position candidates** — the module-filtered set the rooting
        // was selected from — so a class/module pair at one FQN across two DLLs
        // is not a contest at all (fsi-verified: the class binds in both
        // reference orders; codex review round 4), while a same-DLL companion
        // module and a `type Alias = Widget` beside a generic `type Alias<'T>`
        // keep resolving as before.
        if self.assemblies.distinct_dlls(&candidates) > 1 {
            let supplied = self.contest_has_a_supplier(
                candidates,
                |handle| {
                    self.assembly_type_path_from_root(
                        &names,
                        segments.len(),
                        base,
                        k,
                        arity,
                        handle,
                    )
                },
                // A candidate whose walk lands on a **module** leaf supplies no
                // *type*: a module is outside the terminal type namespace at
                // every depth, so it must not contest another DLL's real nested
                // type (codex review round 5).
                |reading| {
                    reading.may_own_path()
                        && !matches!(
                            reading,
                            AssemblyPath::Resolved { payload, .. }
                                if payload.leaf.is_some_and(|leaf| {
                                    self.assemblies.is_authoritative_module(leaf)
                                })
                        )
                },
            );
            return if supplied {
                AssemblyPath::ContestedRooting
            } else {
                AssemblyPath::Resolved {
                    payload: TypePathReading {
                        idx_recs: (0..segments.len())
                            .map(|idx| (idx, Resolution::Deferred(DeferredReason::QualifiedAccess)))
                            .collect(),
                        // A partial reading names no whole-path type; here the
                        // rooting is unnameable too.
                        leaf: None,
                    },
                    owns_path: false,
                }
            };
        }

        self.assembly_type_path_from_root(&names, segments.len(), base, k, arity, type_handle)
    }

    /// One reading's walk *below* an already-chosen rooting type — the body of
    /// [`Self::assembly_type_path_core`] past its longest-public-type-prefix
    /// search, parameterised by the rooting `type_handle` so a **contested**
    /// rooting can ask the same question of each contestant rather than of the
    /// first-wins slot alone (the type-path mirror of
    /// [`Self::assembly_path_records_from_root`]).
    ///
    /// `names` is the full prefix-plus-segments path, `segment_count` the
    /// source-segment count `idx_recs` is keyed against, `base` the prefix
    /// length, `k` the index of the rooting segment, and `arity` the generic
    /// arity that applies to the path's final segment. Never returns
    /// [`AssemblyPath::NoMatch`]: the rooting is given.
    fn assembly_type_path_from_root(
        &self,
        names: &[String],
        segment_count: usize,
        base: usize,
        k: usize,
        arity: usize,
        type_handle: EntityHandle,
    ) -> AssemblyPath<TypePathReading> {
        let n = names.len();
        let arity_at = |k: usize| if k == n - 1 { arity } else { 0 };

        // A type-abbreviation *marker* (a metadata-invisible F# abbreviation
        // surfaced name-only from the signature pickle): the name is really
        // taken — FCS binds the abbreviation here — and a decoded, resolvable
        // target lets the path resolve exactly as FCS does. The marker is the
        // recorded entity for its own segment (and the leaf, when the path
        // ends on it — FCS names the abbreviation, not its target); the
        // chased terminal only carries the walk PAST it. An unchaseable target
        // ([`AssemblyEnv::resolve_abbreviation_tycon`] declines structural /
        // unloaded / ambiguous shapes) keeps the pre-chase shadow-defer
        // instead.
        let walk_root = if self.assemblies.is_abbreviation(type_handle) {
            match self.assemblies.resolve_abbreviation_tycon(type_handle) {
                Some(terminal) => terminal,
                None => return AssemblyPath::Occupied(DeclineCause::AliasTargetUnchaseable),
            }
        } else {
            type_handle
        };

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
        // `named` tracks the entity the latest segment NAMES (the marker at an
        // abbreviation segment), `parent` the entity the walk continues on (its
        // chased terminal) — they differ only across an abbreviation.
        // `via_alias`: once the path roots (or descends) through a *resolved*
        // alias, FCS owns the reading — an absent child past it must DEFER,
        // never cede ownership to a lower-priority open's same-named type:
        // the child may exist on a surface the projection dropped, so absence
        // from the tree does not prove absence (codex round 5; the type-path
        // mirror of the value walk's `via_alias` rule).
        let mut via_alias = walk_root != type_handle;
        let mut named = type_handle;
        let mut parent = walk_root;
        let mut i = k + 1;
        let mut owns_path = true;
        while i < n {
            if let Some(child) = self
                .assemblies
                .nested(parent, &names[i], arity_at(i))
                .filter(|&h| self.assemblies.is_public(h))
            {
                // A nested abbreviation marker (`Lib.Auto.Foo` where `Foo` is
                // a module-scoped abbreviation): same chase-or-defer as the
                // rooting case above. (A nested alias below a *merged*
                // non-alias container cannot be reached here — a rooting two
                // DLLs export already deferred the whole path above.)
                let next = if self.assemblies.is_abbreviation(child) {
                    match self.assemblies.resolve_abbreviation_tycon(child) {
                        Some(terminal) => {
                            via_alias = true;
                            terminal
                        }
                        None => {
                            return AssemblyPath::Occupied(DeclineCause::AliasTargetUnchaseable);
                        }
                    }
                } else {
                    child
                };
                idx_recs.push((i - base, Resolution::Entity(child)));
                named = child;
                parent = next;
                i += 1;
            } else if via_alias {
                return AssemblyPath::Occupied(DeclineCause::AliasOwnedTail);
            } else {
                owns_path = false;
                break;
            }
        }
        // An unresolvable tail (a nested type we don't model, or a non-type
        // segment) is modeled-but-unresolved: defer, never drop.
        for idx in (i - base)..segment_count {
            idx_recs.push((idx, deferred));
        }
        AssemblyPath::Resolved {
            payload: TypePathReading {
                idx_recs,
                // The whole-path type, exactly when the reading owns the path
                // (the walk reached the final segment); a partial reading has
                // no whole-path type to name. This is the entity FCS *names*
                // for the path — an abbreviation itself, never its target.
                leaf: owns_path.then_some(named),
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
    /// public nested types — a type iff that consumes the whole path. An
    /// abbreviation marker at a **non-final** segment descends on its chased
    /// terminal (`open type Lib.Env.SpecialFolder` where `type Env =
    /// System.Environment` — FCS chases `Env` before finding the nested
    /// enum; a marker has no nested types of its own, and an unchaseable one
    /// keeps the descent failing). A *final*-segment marker is returned
    /// as-is: each caller decides what a marker leaf means (the `open type`
    /// consumer chases it, the plain-open classifier keeps it opaque).
    pub(super) fn opened_assembly_type(&self, path: &[String]) -> Option<EntityHandle> {
        let n = path.len();
        let (k, mut handle) = (0..n).rev().find_map(|k| {
            self.assemblies
                .lookup_type(&path[..k], &path[k], 0)
                .filter(|&h| self.assemblies.is_public(h))
                .map(|h| (k, h))
        })?;
        for seg in &path[k + 1..] {
            let parent = if self.assemblies.is_abbreviation(handle) {
                // The reference-order collision guard: a chase at or below a
                // rooting two DLLs export must not descend out of the
                // first-indexed subtree (codex round 3).
                if self.assemblies.alias_rooting_collides_across_dlls(handle) {
                    return None;
                }
                self.assemblies.resolve_abbreviation_target(handle)?
            } else {
                handle
            };
            handle = self
                .assemblies
                .nested(parent, seg, 0)
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
    /// ([`AssemblyEnv::is_require_qualified_access`](crate::AssemblyEnv::is_require_qualified_access) is the signal).
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
    /// at that prefix ([`AssemblyEnv::public_entities_named`](crate::AssemblyEnv::public_entities_named)) rather than the
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
    /// 3. the **implicit opens** — `FSharp.Core`'s seed and the manifest
    ///    `[<assembly: AutoOpen>]` surfaces;
    /// 4. **root / fully-qualified** — `open type Demo.Calc`, or a bare root type.
    ///
    /// An explicit `open` outranks the enclosing namespace, which outranks the
    /// root: in `namespace Demo` with `open Demo.Sub`, `open type Calc` binds
    /// `Demo.Sub.Calc` (the open), not `Demo.Calc`; and `open Demo; open type
    /// Calc` binds `Demo.Calc`, not a root `Calc`.
    ///
    /// This is [`Self::assembly_prefixes_by_priority`]'s ladder — one precedence
    /// law, spelled here over the enclosing *nesting* rather than the single
    /// enclosing namespace because an `open type` may be written relative to any
    /// enclosing module. A tier that moves there moves here.
    ///
    /// Shadowing uses [`Self::open_type_target_shadowed`] — the type-namespace
    /// check (a project value of the same name does not shadow a type), widened
    /// by the augmentation head that gates the *statics* this open enumerates.
    /// `None` (an *opaque* open: bare-name resolution
    /// stays conservative) when the target is project-shadowed, names no
    /// accessible assembly type, or is ambiguous across distinct opens (F# breaks
    /// that by latest-open precedence, which we do not model, so we decline rather
    /// than guess). The explicit-open and enclosing-namespace tiers are suppressed
    /// while an [`unmodelled_open_active`](Self::unmodelled_open_active) prior open
    /// could shorten the name through a path we cannot see; a fully-qualified
    /// path (tier 3) needs no open, so it is still honoured.
    pub(super) fn opened_type_target(&self, path: &[String]) -> Option<EntityHandle> {
        // One open stratum's readings, in the shared order (latest-open-first;
        // within an open relative-before-root), mirroring
        // [`Self::resolve_assembly_path_tiered`]. So in `namespace Demo; open
        // Sub`, an `open type` target only in the root `Sub` (`RootOnly`)
        // resolves through the open's root reading — without it the open would
        // wrongly go opaque and suppress later opened statics — while a
        // colliding name takes the relative `Demo.Sub`. The latest open with a
        // match wins. `Some(verdict)` is this stratum's answer and stops the
        // walk; `None` means no reading here spoke.
        let through_opens = |prefixes: &mut dyn Iterator<Item = (DeclineTier, &[String])>| {
            for (_, prefix) in prefixes {
                let mut full = prefix.to_vec();
                full.extend_from_slice(path);
                if self.open_type_target_shadowed(&full) {
                    return Some(None); // an open routes it into project territory
                }
                if let Some(handle) = self.opened_assembly_type(&full) {
                    return Some(Some(handle));
                }
            }
            None
        };
        // The shortening tiers (opens, enclosing namespace) only when no
        // unmodelled open could invisibly provide the name.
        if !self.unmodelled_open_active {
            // Tier 1 — explicit source opens.
            if let Some(verdict) = through_opens(&mut self.explicit_open_reading_prefixes()) {
                return verdict;
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
                    return (!self.open_type_target_shadowed(&full)).then_some(handle);
                }
            }
            // Tier 3 — the implicit opens, which the enclosing nesting outranks.
            if let Some(verdict) = through_opens(&mut self.implicit_open_reading_prefixes()) {
                return verdict;
            }
        }
        // Tier 4 — the path as written, from the root (a fully-qualified path, or
        // a bare root-namespace type). Lowest precedence, so a shortenable name
        // resolves through tiers 1–3 first.
        //
        // Suppressed for a *bare* single-segment name inside an enclosing
        // namespace: a project type of that name declared in the namespace would
        // shadow a root type, but we do not index project *types* across files, so
        // a cross-file `namespace Demo; type Calc` is invisible here. Resolving the
        // root type could therefore be wrong — decline (defer) rather than guess.
        let bare_in_namespace = path.len() == 1 && !self.container_path.is_empty();
        if !bare_in_namespace && let Some(handle) = self.opened_assembly_type(path) {
            return (!self.open_type_target_shadowed(path)).then_some(handle);
        }
        None
    }

    /// The `open type` flavour of [`Self::path_is_project_type_shadowed`]: also
    /// declines when project source **augments** the target
    /// ([`Self::path_is_augmentation_head_shadowed`]).
    ///
    /// The type *name* an augmentation head writes is unshadowed — the head
    /// declares no type — but this predicate does not gate the name. It gates
    /// enumerating the type's **statics** into unqualified scope, and an
    /// augmentation contributes statics to exactly that surface. fsi, with
    /// `type Demo.Calc with static member Zero (x: int) = x + 1000 / static
    /// member Fresh = 7` ahead of `open type Demo.Calc`: bare `Zero 5` is 1005
    /// (the extension wins the group) and bare `Fresh` binds only through it.
    /// Committing the assembly's statics there is a wrong target, so the open
    /// goes opaque.
    fn open_type_target_shadowed(&self, names: &[String]) -> bool {
        self.path_is_project_type_shadowed(names) || self.path_is_augmentation_head_shadowed(names)
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
    /// [`Preceding`]: super::model::ProjectItems
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
