//! What the walk knows — or does not — about a property's stored value.
//!
//! Three forward-uncertainty channels ride alongside the property table, and
//! they are genuinely different downstream, so they are a *product*, not a
//! collapsed flag:
//!
//! * **unpinned** rides the diagnostic pipeline. A read re-surfaces its root
//!   under the active context, so it can flip `items_uncertain` and
//!   `define_constants_uncertain`.
//! * **sdk_package** is a silent marker checked only at package/item sites,
//!   propagating through *clean* reads. It deliberately never reaches
//!   `items_uncertain` — Compile evaluation tolerates SDK property machinery.
//! * **refused** records that we stored *no* value where the real build stored
//!   one. It is read where a name's current value decides something.
//!
//! [`Trust`] holds all three at once, which is the point: the channels are
//! updated together at a write ([`PropertyProvenance`]) and consulted together
//! at a read, so a code path cannot service one and silently forget the rest.
//! Where a reader wants a *subset* — and two of them legitimately do — it says
//! so by calling a named projection, which is a visible decision rather than a
//! quietly short list of map lookups.
//!
//! The fields are private to this module, so certainty cannot be fabricated by
//! writing a struct literal: it is reachable only through [`Trust::CERTAIN`],
//! and through joins and clears that name what they are doing.

use std::ops::Range;

use crate::diagnostic::{DiagnosticKind, DiagnosticOrigin};

/// Why a stored property value cannot be trusted as final: the divergence a
/// real build could introduce, re-surfaced as a diagnostic at every read of
/// the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnpinnedRoot {
    /// The value chain substituted `$(Name)` while `Name` was undefined —
    /// a real build may supply it (environment variables are MSBuild
    /// initial properties) and produce a different value.
    Undefined(String),
    /// A write in the value chain was gated on a condition outside the
    /// supported grammar, so we cannot know whether it ran.
    UnsupportedCondition(String),
}

impl UnpinnedRoot {
    pub(crate) fn to_diagnostic(&self) -> DiagnosticKind {
        match self {
            UnpinnedRoot::Undefined(name) => {
                DiagnosticKind::UndefinedProperty { name: name.clone() }
            }
            UnpinnedRoot::UnsupportedCondition(condition) => DiagnosticKind::UnsupportedCondition {
                condition: condition.clone(),
            },
        }
    }
}

/// Where a write we cannot trust for a later package read happened. Carried so
/// the package-side decline can point at the write rather than at the read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SdkPackagePropertyTaint {
    pub(crate) span: Range<usize>,
    pub(crate) origin: DiagnosticOrigin,
}

/// The channel-wise certainty of a [`Trust`], with the witnesses dropped.
///
/// The witnesses are *evidence*, not part of the verdict: two joins that keep
/// different first causes still say the same thing about trustworthiness. The
/// algebra's commutativity holds at this granularity and not finer, so this is
/// the currency the laws are stated in — which is the whole of its use, hence
/// test-only.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrustShape {
    unpinned: bool,
    sdk_package: bool,
    refused: bool,
}

/// The three forward-uncertainty channels for one property name.
///
/// [`Trust::CERTAIN`] is the identity of [`Trust::join`] and means every
/// channel is clean — the walk's value for the name is the real build's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Trust {
    unpinned: Option<UnpinnedRoot>,
    sdk_package: Option<SdkPackagePropertyTaint>,
    refused: bool,
}

impl Trust {
    /// Every channel clean. The identity of [`Trust::join`], and the only
    /// certainty this module hands out.
    pub(crate) const CERTAIN: Self = Trust {
        unpinned: None,
        sdk_package: None,
        refused: false,
    };

    /// [`Trust::CERTAIN`], borrowed — what a lookup that finds no entry should
    /// hand back, so a reader can ask its question of a `&Trust` either way
    /// instead of open-coding "absent means clean".
    pub(crate) fn certain_ref() -> &'static Trust {
        static CERTAIN: Trust = Trust::CERTAIN;
        &CERTAIN
    }

    /// Nothing to declare on any channel.
    ///
    /// Destructured rather than field-accessed, here and in every other
    /// projection, so that a fourth channel is a compile error at each place
    /// that would otherwise ignore it. The channel this type exists to keep in
    /// step is precisely one that was added outside the discipline and drifted
    /// for as long as it stayed there.
    pub(crate) fn is_certain(&self) -> bool {
        let Trust {
            unpinned,
            sdk_package,
            refused,
        } = self;
        unpinned.is_none() && sdk_package.is_none() && !refused
    }

    #[cfg(test)]
    pub(crate) fn shape(&self) -> TrustShape {
        let Trust {
            unpinned,
            sdk_package,
            refused,
        } = self;
        TrustShape {
            unpinned: unpinned.is_some(),
            sdk_package: sdk_package.is_some(),
            refused: *refused,
        }
    }

    /// Combine two names' trust, channel by channel — what a value referencing
    /// both of them carries.
    ///
    /// **Left-biased**, deliberately: where both sides have a witness the
    /// left one survives, so folding over a value's references in source order
    /// reports the *first* offending reference. That is the diagnostic a reader
    /// of the value wants, and it is why the join is not commutative on the
    /// nose — only on its channel-wise shape.
    pub(crate) fn join(self, other: Trust) -> Trust {
        Trust {
            unpinned: self.unpinned.or(other.unpinned),
            sdk_package: self.sdk_package.or(other.sdk_package),
            refused: self.refused || other.refused,
        }
    }

    /// The root cause behind a stored value the property pass could not pin
    /// down: the value chain substituted an undefined reference, read another
    /// unpinned property, or a write in the chain sat behind a gate we couldn't
    /// evaluate.
    ///
    /// The stored value is our best evaluation, but a real build can diverge,
    /// so every read — a `$(…)` expansion or a condition — re-surfaces this
    /// root as a diagnostic under the active contexts, degrading compile and
    /// package certainty exactly like a direct undefined reference. A later
    /// clean overwrite re-pins the property.
    pub(crate) fn unpinned(&self) -> Option<&UnpinnedRoot> {
        self.unpinned.as_ref()
    }

    /// Where the write came from, when the name's current value came from the
    /// SDK property pass or from an SDK property write evaluated with an
    /// untrusted condition or value.
    ///
    /// The taint is package-specific: Compile uncertainty deliberately
    /// tolerates SDK property machinery, but a later
    /// `<PackageReference Version="$(Name)">` consuming such a value must not
    /// be reported as trustworthy, because MSBuild evaluates project properties
    /// before project items.
    pub(crate) fn sdk_package(&self) -> Option<&SdkPackagePropertyTaint> {
        self.sdk_package.as_ref()
    }

    /// Whether the name's write was *refused* — the value carried an
    /// expression, item reference, or metadata reference we can't evaluate, so
    /// the binding was removed rather than stored.
    ///
    /// The real build stores that value, so a later undefined read of the name
    /// is not exact, and neither is a splice decision resting on it. The mark
    /// then stands for the rest of the walk: a later clean write does *not*
    /// discharge it, because its reader consumes it mid-walk and tolerates
    /// SDK-subtree opacity, under which a cleanly-computed value can still rest
    /// on a property that hidden content redefined (see [`RefusedOutcome::Keep`]
    /// at the clean-write site). The one clear is the reserved-toolset seed,
    /// which re-establishes a name the real build refuses to let the document
    /// write at all.
    pub(crate) fn refused(&self) -> bool {
        self.refused
    }

    /// Whether a *reference* to this name is untrustworthy — the two channels
    /// that describe a value we hold but may have wrong.
    ///
    /// The refused channel is deliberately absent: it says we hold *no* value,
    /// which surfaces to a reader as the name being undefined and is judged
    /// there ([`Trust::refused`], consulted by the undefined-read guard). Every
    /// site that sets it drops the binding first, so there is no value left for
    /// a reference to be untrusting *of* — that pairing is enforced at the
    /// write, not by this type.
    pub(crate) fn reference_is_untrusted(&self) -> bool {
        let Trust {
            unpinned,
            sdk_package,
            refused: _,
        } = self;
        unpinned.is_some() || sdk_package.is_some()
    }

    /// Apply one write's verdict, returning the name's resulting trust.
    ///
    /// Not a join: [`TaintOutcome::Clear`] and its siblings are deliberately
    /// *non-monotone*, because a clean write genuinely re-pins a name. The join
    /// combines what several names contribute to one value; this combines what
    /// several writes do to one name, and only the former is a lattice.
    pub(crate) fn after(mut self, provenance: ResolvedProvenance) -> Trust {
        // Destructured for the same reason the projections above are: a
        // channel added to the verdict must be handled here, not defaulted.
        let PropertyProvenance {
            taint,
            unpinned,
            refused,
        } = provenance;
        match taint {
            TaintOutcome::Set(taint) => self.sdk_package = Some(taint),
            TaintOutcome::Clear => self.sdk_package = None,
            TaintOutcome::Keep => {}
        }
        match unpinned {
            UnpinnedOutcome::Set(root) => self.unpinned = Some(root),
            UnpinnedOutcome::Clear => self.unpinned = None,
            UnpinnedOutcome::Keep => {}
        }
        match refused {
            RefusedOutcome::Set => self.refused = true,
            RefusedOutcome::Clear => self.refused = false,
            RefusedOutcome::Keep => {}
        }
        self
    }
}

/// What a single property write does to the SDK-package taint channel.
pub(crate) enum TaintOutcome<T> {
    /// Mark the property tainted (a write we can't trust for a later package
    /// read). Carries the write's span at the call site, and the span/origin
    /// pair resolved against the current file once the walk resolves it.
    Set(T),
    /// Clear any existing taint — a clean write re-pins the name.
    Clear,
    /// Leave the existing taint mark as-is.
    Keep,
}

/// What a single property write does to the unpinned channel.
pub(crate) enum UnpinnedOutcome {
    /// Record `root` — every later read re-surfaces it as a diagnostic.
    Set(UnpinnedRoot),
    /// Clear any existing root — a clean write under a clean gate re-pins.
    Clear,
    /// Leave the existing root as-is.
    Keep,
}

/// What a single property write does to the refused-write channel.
pub(crate) enum RefusedOutcome {
    /// Record the refusal — we stored no value, so the one the real build
    /// stored is unknown to us.
    Set,
    /// Discharge any earlier refusal — this write re-pins the name.
    Clear,
    /// Leave the existing mark as-is.
    Keep,
}

/// The provenance verdict for a single property write: what happens to **all
/// three** channels. Because a write must name an outcome for each, a new
/// write path cannot update one channel and silently forget the others.
///
/// A write site spells the taint outcome with the raw span it has to hand; the
/// walk resolves that to a [`SdkPackagePropertyTaint`] — remapped span plus
/// origin — with [`PropertyProvenance::map_taint`], and only the resolved form
/// reaches [`Trust::after`].
pub(crate) struct PropertyProvenance<T = Range<usize>> {
    pub(crate) taint: TaintOutcome<T>,
    pub(crate) unpinned: UnpinnedOutcome,
    pub(crate) refused: RefusedOutcome,
}

/// A [`PropertyProvenance`] whose taint payload has been resolved against the
/// file the walk is currently in.
pub(crate) type ResolvedProvenance = PropertyProvenance<SdkPackagePropertyTaint>;

impl<T> PropertyProvenance<T> {
    /// Resolve the taint payload, leaving the other two channels' verdicts
    /// untouched.
    pub(crate) fn map_taint<U>(self, f: impl FnOnce(T) -> U) -> PropertyProvenance<U> {
        PropertyProvenance {
            taint: self.taint.map(f),
            unpinned: self.unpinned,
            refused: self.refused,
        }
    }
}

impl<T> TaintOutcome<T> {
    /// The taint outcome of a write the property pass performed: taint it
    /// when the value/condition is untrusted, else clear unless a prior
    /// taint must be preserved (an earlier untrusted write to the same
    /// name whose divergence still stands).
    pub(crate) fn after_write(taints_property: bool, payload: T, preserve_existing: bool) -> Self {
        if taints_property {
            TaintOutcome::Set(payload)
        } else if preserve_existing {
            TaintOutcome::Keep
        } else {
            TaintOutcome::Clear
        }
    }

    /// Replace the payload, keeping the verdict — how a raw span becomes a
    /// span/origin pair once the walk knows which file it is in.
    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> TaintOutcome<U> {
        match self {
            TaintOutcome::Set(payload) => TaintOutcome::Set(f(payload)),
            TaintOutcome::Clear => TaintOutcome::Clear,
            TaintOutcome::Keep => TaintOutcome::Keep,
        }
    }
}

impl UnpinnedOutcome {
    /// The unpinned outcome of a write the property pass performed:
    /// `unpinned_by` is the root cause when the new value (or the gate it
    /// sat behind) leans on one; a clean value under a clean gate re-pins,
    /// while a clean value under a still-uncertain gate leaves the prior
    /// state untouched.
    pub(crate) fn after_write(
        unpinned_by: Option<UnpinnedRoot>,
        write_condition_maybe_wrong: bool,
    ) -> Self {
        match unpinned_by {
            Some(root) => UnpinnedOutcome::Set(root),
            None if !write_condition_maybe_wrong => UnpinnedOutcome::Clear,
            None => UnpinnedOutcome::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn unpinned_root_strategy() -> impl Strategy<Value = UnpinnedRoot> {
        prop_oneof![
            "[ab]".prop_map(UnpinnedRoot::Undefined),
            "[ab]".prop_map(UnpinnedRoot::UnsupportedCondition),
        ]
    }

    fn taint_strategy() -> impl Strategy<Value = SdkPackagePropertyTaint> {
        (
            0usize..3,
            prop_oneof![
                Just(DiagnosticOrigin::Buffer),
                Just(DiagnosticOrigin::Imported)
            ],
        )
            .prop_map(|(start, origin)| SdkPackagePropertyTaint {
                span: start..start + 1,
                origin,
            })
    }

    /// A deliberately tiny alphabet: the interesting cases are the ones where
    /// two operands both carry a witness and the join has to pick, so the
    /// witnesses must collide often.
    fn trust_strategy() -> impl Strategy<Value = Trust> {
        (
            proptest::option::of(unpinned_root_strategy()),
            proptest::option::of(taint_strategy()),
            any::<bool>(),
        )
            .prop_map(|(unpinned, sdk_package, refused)| Trust {
                unpinned,
                sdk_package,
                refused,
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// The soundness law, and the only one that would let a defect through:
        /// a join may not manufacture certainty. Everything else here is about
        /// the join being a sane thing to fold with; this is about it being
        /// *safe* to fold with.
        #[test]
        fn join_is_certain_only_when_both_sides_are(
            a in trust_strategy(),
            b in trust_strategy(),
        ) {
            let both_certain = a.is_certain() && b.is_certain();
            prop_assert_eq!(a.join(b).is_certain(), both_certain);
        }

        /// The fold `Trust::CERTAIN` starts from must not be a source of
        /// certainty either: a value referencing any untrusted name is
        /// untrusted, however many trusted names surround it. This is the law
        /// as the raw-text scan actually uses it.
        #[test]
        fn folding_over_references_is_certain_only_when_every_one_is(
            parts in proptest::collection::vec(trust_strategy(), 0..6),
        ) {
            let every_part_certain = parts.iter().all(Trust::is_certain);
            let folded = parts.into_iter().fold(Trust::CERTAIN, Trust::join);
            prop_assert_eq!(folded.is_certain(), every_part_certain);
        }

        #[test]
        fn certain_is_a_two_sided_identity(a in trust_strategy()) {
            prop_assert_eq!(a.clone().join(Trust::CERTAIN), a.clone());
            prop_assert_eq!(Trust::CERTAIN.join(a.clone()), a);
        }

        #[test]
        fn join_is_idempotent(a in trust_strategy()) {
            prop_assert_eq!(a.clone().join(a.clone()), a);
        }

        #[test]
        fn join_is_associative(
            a in trust_strategy(),
            b in trust_strategy(),
            c in trust_strategy(),
        ) {
            prop_assert_eq!(
                a.clone().join(b.clone()).join(c.clone()),
                a.join(b.join(c)),
            );
        }

        /// Commutative on the verdict, and *only* on the verdict — the
        /// witnesses are first-cause-wins, so the operand order decides which
        /// one is reported. Pinned at this granularity on purpose: a stronger
        /// claim would be false, and a weaker one would not rule out an
        /// order-dependent verdict.
        #[test]
        fn join_is_commutative_on_the_verdict(
            a in trust_strategy(),
            b in trust_strategy(),
        ) {
            prop_assert_eq!(
                a.clone().join(b.clone()).shape(),
                b.join(a).shape(),
            );
        }

        /// The left bias is load-bearing for diagnostics, so it is pinned
        /// rather than left as an accident of the implementation.
        #[test]
        fn join_keeps_the_left_witness(
            a in trust_strategy(),
            b in trust_strategy(),
        ) {
            let joined = a.clone().join(b.clone());
            if a.unpinned().is_some() {
                prop_assert_eq!(joined.unpinned(), a.unpinned());
            } else {
                prop_assert_eq!(joined.unpinned(), b.unpinned());
            }
            if a.sdk_package().is_some() {
                prop_assert_eq!(joined.sdk_package(), a.sdk_package());
            } else {
                prop_assert_eq!(joined.sdk_package(), b.sdk_package());
            }
        }

        /// `reference_is_untrusted` is a *subset* projection, and the subset is
        /// the decision. Pinned in both directions so that widening it — the
        /// obvious "tidy-up", and a behaviour change — cannot pass silently.
        #[test]
        fn a_reference_is_untrusted_by_the_two_value_channels_only(a in trust_strategy()) {
            prop_assert_eq!(
                a.reference_is_untrusted(),
                a.unpinned().is_some() || a.sdk_package().is_some(),
            );
            let refusal_alone = Trust {
                unpinned: None,
                sdk_package: None,
                refused: true,
            };
            prop_assert!(!refusal_alone.reference_is_untrusted());
            prop_assert!(!refusal_alone.is_certain());
        }
    }

    /// Every channel must be reachable from a certain start, or a test suite
    /// that only ever exercises two of three would look complete.
    #[test]
    fn each_channel_alone_defeats_certainty() {
        for provenance in [
            ResolvedProvenance {
                taint: TaintOutcome::Set(SdkPackagePropertyTaint {
                    span: 0..1,
                    origin: DiagnosticOrigin::Buffer,
                }),
                unpinned: UnpinnedOutcome::Keep,
                refused: RefusedOutcome::Keep,
            },
            ResolvedProvenance {
                taint: TaintOutcome::Keep,
                unpinned: UnpinnedOutcome::Set(UnpinnedRoot::Undefined("X".to_string())),
                refused: RefusedOutcome::Keep,
            },
            ResolvedProvenance {
                taint: TaintOutcome::Keep,
                unpinned: UnpinnedOutcome::Keep,
                refused: RefusedOutcome::Set,
            },
        ] {
            assert!(
                !Trust::CERTAIN.after(provenance).is_certain(),
                "a write that sets one channel must leave the name uncertain"
            );
        }
    }

    /// `after` is where the non-monotone half lives: a clean write re-pins.
    /// Asserted per channel so a clear that services two channels and misses
    /// the third fails here rather than in a differential.
    #[test]
    fn a_clearing_write_restores_certainty_on_every_channel() {
        let dirty = Trust::CERTAIN.after(ResolvedProvenance {
            taint: TaintOutcome::Set(SdkPackagePropertyTaint {
                span: 0..1,
                origin: DiagnosticOrigin::Buffer,
            }),
            unpinned: UnpinnedOutcome::Set(UnpinnedRoot::Undefined("X".to_string())),
            refused: RefusedOutcome::Set,
        });
        assert_eq!(
            dirty.shape(),
            TrustShape {
                unpinned: true,
                sdk_package: true,
                refused: true
            }
        );
        let cleared = dirty.after(ResolvedProvenance {
            taint: TaintOutcome::Clear,
            unpinned: UnpinnedOutcome::Clear,
            refused: RefusedOutcome::Clear,
        });
        assert_eq!(cleared, Trust::CERTAIN);
    }
}
