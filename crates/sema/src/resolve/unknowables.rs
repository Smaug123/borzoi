//! The file-wide **unknowables** ([`Unknowables`]): the reasons the walk has
//! found, anywhere in the file so far, that some question about the whole
//! file has no enumerable answer.
//!
//! Each reason is raised where the walk meets the construct that makes it so,
//! and a raised reason stays raised for the rest of the file: there is
//! [`Unknowables::raise`] and no way to lower one. A reader asks
//! [`Unknowables::contains`], so a site that defers on a reason cannot read
//! a value that some later code reset.
//!
//! What a pass over the whole file decides before the walk starts is not an
//! unknowable the walk raises; it is a [`Prescan`](super::prescan::Prescan)
//! fact, which the walk borrows and cannot write.

/// One reason a file-wide question has no enumerable answer; see the
/// [module docs](self).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unknowable {
    /// EX-2: some explicit `open` in the file brings an extension surface
    /// whose names cannot be enumerated here: a project module or namespace
    /// (EX-3), an assembly module or `open type`, or an opaque, vetoed or
    /// dropped-path open. The overload engine's extension gate then defers
    /// every method-call commit in the file, as it did for *any* `open`
    /// before EX-2. File-global like
    /// [`Resolver::open_extension_namespaces`](super::state::Resolver::open_extension_namespaces):
    /// an `open` in a nested module counts for the whole file.
    OpenExtensionSurface,
    /// Some attribute in the file has no resolvable *name shape*, a nameless
    /// `[<>]` or an ident-less path, so the extension gate cannot key it and
    /// keeps the presence defer (EX-3 §2(d) stage 5).
    AttributeShape,
    /// Some `type … with` augmentation member's *name* is not walkable (an
    /// operator head, an `inherit`), so the extension gate keeps the
    /// wholesale defer (EX-3 §2(a)).
    AugmentationNames,
    /// An earlier `namespace global` block of this file held an `open`. FCS
    /// carries what such an open brings into the blocks after it (probed: a
    /// later block's `Alpha.Color.Red` binds the opened module's `Alpha`
    /// over a same-file `namespace Alpha`), which the per-block open state
    /// does not model.
    GlobalBlockOpen,
}

impl Unknowable {
    /// Every reason, each once.
    #[cfg(test)]
    pub(super) const ALL: [Unknowable; 4] = [
        Unknowable::OpenExtensionSurface,
        Unknowable::AttributeShape,
        Unknowable::AugmentationNames,
        Unknowable::GlobalBlockOpen,
    ];

    /// The reason's bit in an [`Unknowables`]. Distinct reasons have distinct
    /// discriminants (the compiler rejects a repeated one), so they have
    /// distinct bits.
    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// The set of reasons raised so far in the file; see the
/// [module docs](self).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Unknowables(u8);

impl Unknowables {
    /// Raise `reason` for the rest of the file.
    pub(super) fn raise(&mut self, reason: Unknowable) {
        self.0 |= reason.bit();
    }

    /// Whether `reason` has been raised.
    pub(super) fn contains(self, reason: Unknowable) -> bool {
        self.0 & reason.bit() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::{Unknowable, Unknowables};
    use proptest::prelude::*;

    fn reason() -> impl Strategy<Value = Unknowable> {
        proptest::sample::select(Unknowable::ALL.to_vec())
    }

    proptest! {
        /// The set holds exactly the reasons raised into it, after every
        /// raise: raising one reason raises no other, and lowers none.
        #[test]
        fn the_set_holds_exactly_the_reasons_raised(
            raised in prop::collection::vec(reason(), 0..12),
        ) {
            let mut set = Unknowables::default();
            for (i, &r) in raised.iter().enumerate() {
                set.raise(r);
                for probe in Unknowable::ALL {
                    prop_assert_eq!(
                        set.contains(probe),
                        raised[..=i].contains(&probe),
                        "after raising {:?}, {:?}",
                        &raised[..=i],
                        probe
                    );
                }
            }
        }
    }
}
