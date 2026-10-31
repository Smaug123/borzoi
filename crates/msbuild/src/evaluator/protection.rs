//! Which property names a project write cannot land on, and how sure the walk
//! is of that.
//!
//! MSBuild discards a project write to a reserved well-known name, and to a
//! caller-supplied global unless `TreatAsLocalProperty` opted it out. An
//! imported root's opt-out lasts for the rest of the evaluation, so content
//! behind an import gate the walk could not decide may have opted a global out
//! or not. Protection is therefore three-valued, and [`ProtectionTable::get`]
//! is the only way to read it: every reader has to say what it does with
//! [`Protection::Undecided`], rather than one side of it being the default a
//! reader gets by forgetting to look.

use std::collections::HashMap;

use crate::trust::UnpinnedRoot;

/// Whether a project write to one name lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Protection {
    /// Discarded in every build the walk could be modelling: a reserved name,
    /// or a caller global nothing has opted out.
    Protected,
    /// Lands in every such build: not a global, or a global content the walk is
    /// certain ran opted out.
    Writable,
    /// Depends on whether content behind an undecided import gate ran, which
    /// opts the name out. `root` is that gate's cause.
    Undecided {
        /// The side the walk took, which its own value table follows. A reader
        /// that has to act on one side acts on this one, and records `root`
        /// against whatever that decides.
        walk_protects: bool,
        root: UnpinnedRoot,
    },
}

impl Protection {
    /// Whether the walk's own value table treats the name as protected.
    pub(super) fn walk_protects(&self) -> bool {
        match self {
            Protection::Protected => true,
            Protection::Writable => false,
            Protection::Undecided { walk_protects, .. } => *walk_protects,
        }
    }
}

#[derive(Clone, Debug)]
enum Entry {
    /// A reserved well-known name. `TreatAsLocalProperty` applies to globals
    /// only, so nothing opts one of these out.
    Reserved,
    /// A caller global the entry document did not opt out.
    Global(GlobalState),
}

#[derive(Clone, Debug)]
enum GlobalState {
    Protected,
    /// Opted out by content the walk is certain ran.
    Writable,
    Undecided {
        walk_protects: bool,
        root: UnpinnedRoot,
    },
}

/// Protection for every name, keyed lowercased (MSBuild compares property names
/// OrdinalIgnoreCase). A name with no entry is an ordinary property: writable.
#[derive(Clone, Debug, Default)]
pub(super) struct ProtectionTable {
    entries: HashMap<String, Entry>,
}

impl ProtectionTable {
    pub(super) fn get(&self, name: &str) -> Protection {
        match self.entries.get(&name.to_ascii_lowercase()) {
            None => Protection::Writable,
            Some(Entry::Reserved | Entry::Global(GlobalState::Protected)) => Protection::Protected,
            Some(Entry::Global(GlobalState::Writable)) => Protection::Writable,
            Some(Entry::Global(GlobalState::Undecided {
                walk_protects,
                root,
            })) => Protection::Undecided {
                walk_protects: *walk_protects,
                root: root.clone(),
            },
        }
    }

    /// Whether `name` is reserved — protected by MSBuild itself rather than by
    /// the caller, so its value is the evaluator's to seed.
    pub(super) fn is_reserved(&self, name: &str) -> bool {
        matches!(
            self.entries.get(&name.to_ascii_lowercase()),
            Some(Entry::Reserved)
        )
    }

    /// Make `name` reserved, whatever it was.
    pub(super) fn reserve(&mut self, name: &str) {
        self.entries
            .insert(name.to_ascii_lowercase(), Entry::Reserved);
    }

    /// Record `name` as a caller global the entry document did not opt out.
    pub(super) fn protect_global(&mut self, name: &str) {
        self.entries
            .entry(name.to_ascii_lowercase())
            .or_insert(Entry::Global(GlobalState::Protected));
    }

    /// A root the walk is following opts `name` out. `gate` is the cause of the
    /// undecided import gate the root is walked under, if any: then the real
    /// build may not have run it.
    pub(super) fn opt_out(&mut self, name: &str, gate: Option<&UnpinnedRoot>) {
        let Some(Entry::Global(state)) = self.entries.get_mut(&name.to_ascii_lowercase()) else {
            return;
        };
        *state = match (&*state, gate) {
            (GlobalState::Writable, _) | (_, None) => GlobalState::Writable,
            (GlobalState::Protected, Some(root)) => GlobalState::Undecided {
                walk_protects: false,
                root: root.clone(),
            },
            (GlobalState::Undecided { root, .. }, Some(_)) => GlobalState::Undecided {
                walk_protects: false,
                root: root.clone(),
            },
        };
    }

    /// Content behind an undecided import gate the walk did *not* follow may
    /// opt `name` out.
    pub(super) fn maybe_opt_out(&mut self, name: &str, root: &UnpinnedRoot) {
        if let Some(Entry::Global(state)) = self.entries.get_mut(&name.to_ascii_lowercase()) {
            Self::maybe_opt_out_state(state, root);
        }
    }

    /// [`Self::maybe_opt_out`] for every caller global, for content the walk
    /// cannot even enumerate.
    pub(super) fn maybe_opt_out_every_global(&mut self, root: &UnpinnedRoot) {
        for entry in self.entries.values_mut() {
            if let Entry::Global(state) = entry {
                Self::maybe_opt_out_state(state, root);
            }
        }
    }

    fn maybe_opt_out_state(state: &mut GlobalState, root: &UnpinnedRoot) {
        if matches!(state, GlobalState::Protected) {
            *state = GlobalState::Undecided {
                walk_protects: true,
                root: root.clone(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// How the walk met one root that opts the global out.
    #[derive(Clone, Copy, Debug)]
    enum Gate {
        /// Ran in every build.
        Certain,
        /// Undecided; the walk followed it.
        Followed,
        /// Undecided; the walk skipped it.
        Skipped,
    }

    fn root(index: usize) -> UnpinnedRoot {
        UnpinnedRoot::UnsupportedCondition(format!("gate{index}"))
    }

    /// The reference: each undecided gate is an unknown boolean, so enumerate
    /// every world. The global is protected in a world iff no opt-out ran in
    /// it, and the walk's world is the one where exactly the followed gates ran.
    fn reference(gates: &[Gate]) -> (bool, bool, bool) {
        let undecided: Vec<usize> = (0..gates.len())
            .filter(|&i| !matches!(gates[i], Gate::Certain))
            .collect();
        let protected_in = |ran: &dyn Fn(usize) -> bool| !(0..gates.len()).any(ran);
        let mut all_protected = true;
        let mut all_writable = true;
        for world in 0..(1u32 << undecided.len()) {
            let ran = |i: usize| match gates[i] {
                Gate::Certain => true,
                _ => world & (1 << undecided.iter().position(|&u| u == i).unwrap()) != 0,
            };
            let protected = protected_in(&ran);
            all_protected &= protected;
            all_writable &= !protected;
        }
        let walk_protects = protected_in(&|i| !matches!(gates[i], Gate::Skipped));
        (all_protected, all_writable, walk_protects)
    }

    proptest! {
        /// For every sequence of opt-outs, the lookup is exactly what the
        /// worlds agree on, and the walk's side when they do not.
        #[test]
        fn lookup_matches_every_world(
            gates in proptest::collection::vec(
                prop_oneof![Just(Gate::Certain), Just(Gate::Followed), Just(Gate::Skipped)],
                0..7,
            ),
        ) {
            let mut table = ProtectionTable::default();
            table.protect_global("G");
            table.reserve("R");
            for (index, gate) in gates.iter().enumerate() {
                for name in ["g", "R", "Other"] {
                    match gate {
                        Gate::Certain => table.opt_out(name, None),
                        Gate::Followed => table.opt_out(name, Some(&root(index))),
                        Gate::Skipped => table.maybe_opt_out(name, &root(index)),
                    }
                }
            }
            let (all_protected, all_writable, walk_protects) = reference(&gates);
            let got = table.get("G");
            match &got {
                Protection::Protected => prop_assert!(all_protected),
                Protection::Writable => prop_assert!(all_writable),
                Protection::Undecided { root, .. } => {
                    prop_assert!(!all_protected && !all_writable);
                    // The cause is one of the undecided gates that opts it out.
                    let from_an_undecided_gate = (0..gates.len())
                        .any(|i| !matches!(gates[i], Gate::Certain) && *root == super::tests::root(i));
                    prop_assert!(from_an_undecided_gate);
                }
            }
            prop_assert_eq!(got.walk_protects(), walk_protects);
            prop_assert_eq!(table.get("r"), Protection::Protected);
            prop_assert_eq!(table.get("other"), Protection::Writable);
        }
    }

    #[test]
    fn unknown_content_reaches_every_global_and_no_reserved_name() {
        let mut table = ProtectionTable::default();
        table.protect_global("A");
        table.protect_global("B");
        table.opt_out("B", None);
        table.reserve("R");
        table.maybe_opt_out_every_global(&root(0));
        assert_eq!(
            table.get("a"),
            Protection::Undecided {
                walk_protects: true,
                root: root(0)
            }
        );
        assert_eq!(table.get("b"), Protection::Writable);
        assert_eq!(table.get("r"), Protection::Protected);
    }
}
