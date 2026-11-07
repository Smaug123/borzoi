//! Which `match` clauses some value can reach.
//!
//! FCS's decision compiler keeps no node or binder of a clause that no value
//! reaches (FS0026, "This rule will never be matched"): every value it would
//! match, an earlier clause without a guard matches first. Inference declines
//! a match with such a clause, so it needs to know which clauses are
//! reachable.
//!
//! [`provably_reachable`] is the usefulness check of Maranget's *Warnings for
//! pattern matching* over [`ClausePat`], under-approximated so that `true` is
//! a proof: a value exists that the clause matches and no earlier clause
//! does. Where the patterns leave it unknown — a constant spelt so that it may
//! equal another (`1` and `0x1`), or a column whose patterns are of different
//! types (ill-typed input) — it answers `false`, which only declines.

/// A pattern as reachability sees it: a constructor applied to its fields'
/// patterns, or a pattern every value matches (a name, `_`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClausePat<U> {
    /// A name or `_`.
    Any,
    /// A constructor and one pattern per field, as many as its arity.
    Ctor(Ctor<U>, Vec<ClausePat<U>>),
}

/// A constructor of a pattern-matched value. `U` names a union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ctor<U> {
    /// `()`, the only value of `unit`.
    Unit,
    /// A tuple of this many elements, the only constructor of its type.
    Tuple(usize),
    /// Case `index` of union `union`, which has `siblings` cases.
    Case {
        union: U,
        index: usize,
        siblings: usize,
    },
    /// `true` or `false`.
    Bool(bool),
    /// A constant of a type with more values than any match spells out
    /// (`int`, `string`, `char`). `value` is its canonical spelling, two of
    /// which are equal exactly when the constants are; `None` for a spelling
    /// that may equal any other constant of its kind.
    Const {
        kind: ConstKind,
        value: Option<Box<str>>,
    },
}

/// The kind of a [`Ctor::Const`]: constants of different kinds belong to
/// different types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConstKind {
    Int,
    Str,
    Char,
}

impl<U: Copy + Eq> Ctor<U> {
    fn arity_of(&self, args: &[ClausePat<U>]) -> usize {
        match self {
            Ctor::Tuple(n) => *n,
            Ctor::Case { .. } => args.len(),
            Ctor::Unit | Ctor::Bool(_) | Ctor::Const { .. } => 0,
        }
    }

    /// Whether both constructors belong to one type, which a well-typed
    /// column's always do.
    fn same_type(&self, other: &Self) -> bool {
        match (self, other) {
            (Ctor::Unit, Ctor::Unit) | (Ctor::Bool(_), Ctor::Bool(_)) => true,
            (Ctor::Tuple(a), Ctor::Tuple(b)) => a == b,
            (Ctor::Case { union: a, .. }, Ctor::Case { union: b, .. }) => a == b,
            (Ctor::Const { kind: a, .. }, Ctor::Const { kind: b, .. }) => a == b,
            _ => false,
        }
    }

    /// Whether a value built by `self` may be one built by `other`: the same
    /// constructor, or constants either of which is spelt non-canonically.
    fn may_equal(&self, other: &Self) -> bool {
        match (self, other) {
            (Ctor::Unit, Ctor::Unit) => true,
            (Ctor::Tuple(a), Ctor::Tuple(b)) => a == b,
            (Ctor::Case { index: a, .. }, Ctor::Case { index: b, .. }) => a == b,
            (Ctor::Bool(a), Ctor::Bool(b)) => a == b,
            (Ctor::Const { value: a, .. }, Ctor::Const { value: b, .. }) => match (a, b) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            },
            _ => false,
        }
    }
}

/// Whether some value matches `clause` and none of `earlier`, the patterns of
/// the clauses before it that have no guard (a guarded clause may fail, so it
/// covers nothing). `false` when that is not provable.
pub(crate) fn provably_reachable<U: Copy + Eq>(
    earlier: &[ClausePat<U>],
    clause: &ClausePat<U>,
) -> bool {
    let rows: Vec<Vec<ClausePat<U>>> = earlier.iter().map(|p| vec![p.clone()]).collect();
    useful(&rows, std::slice::from_ref(clause))
}

/// Maranget's `U(P, q)`: whether some value vector matches `q` and no row of
/// `rows`, every row and `q` of one length.
fn useful<U: Copy + Eq>(rows: &[Vec<ClausePat<U>>], q: &[ClausePat<U>]) -> bool {
    let Some((head, rest)) = q.split_first() else {
        return rows.is_empty();
    };
    match head {
        ClausePat::Ctor(c, args) => {
            let Some(rows) = specialise(rows, c, args.len()) else {
                return false;
            };
            let q: Vec<ClausePat<U>> = args.iter().chain(rest).cloned().collect();
            useful(&rows, &q)
        }
        ClausePat::Any => {
            let mut sigma: Vec<(&Ctor<U>, usize)> = Vec::new();
            for row in rows {
                if let ClausePat::Ctor(c, args) = &row[0] {
                    if sigma.first().is_some_and(|(s, _)| !s.same_type(c)) {
                        return false;
                    }
                    if !sigma.iter().any(|(s, _)| *s == c) {
                        sigma.push((c, c.arity_of(args)));
                    }
                }
            }
            if complete(&sigma) {
                sigma.iter().any(|&(c, arity)| {
                    let Some(rows) = specialise(rows, c, arity) else {
                        return false;
                    };
                    let q: Vec<ClausePat<U>> = std::iter::repeat_n(ClausePat::Any, arity)
                        .chain(rest.iter().cloned())
                        .collect();
                    useful(&rows, &q)
                })
            } else {
                // Some constructor no row names: a value built by it matches
                // exactly the rows whose head matches everything.
                let rows: Vec<Vec<ClausePat<U>>> = rows
                    .iter()
                    .filter(|row| row[0] == ClausePat::Any)
                    .map(|row| row[1..].to_vec())
                    .collect();
                useful(&rows, rest)
            }
        }
    }
}

/// Whether the constructors heading a column (each with its arity) build
/// every value of their type. Constants of an open kind never do.
fn complete<U: Copy + Eq>(sigma: &[(&Ctor<U>, usize)]) -> bool {
    let Some(&(first, _)) = sigma.first() else {
        return false;
    };
    match first {
        Ctor::Unit | Ctor::Tuple(_) => true,
        Ctor::Bool(_) => [true, false]
            .iter()
            .all(|b| sigma.iter().any(|(c, _)| **c == Ctor::Bool(*b))),
        Ctor::Case { siblings, .. } => (0..*siblings).all(|i| {
            sigma
                .iter()
                .any(|(c, _)| matches!(c, Ctor::Case { index, .. } if *index == i))
        }),
        Ctor::Const { .. } => false,
    }
}

/// Maranget's `S(c, P)`: the rows that may match a value built by `c`, each
/// with its head replaced by that value's `arity` fields. A row that may or
/// may not match it is kept, so the result can only make `q` less useful.
/// `None` when a head is of another type than `c`.
fn specialise<U: Copy + Eq>(
    rows: &[Vec<ClausePat<U>>],
    c: &Ctor<U>,
    arity: usize,
) -> Option<Vec<Vec<ClausePat<U>>>> {
    let mut out = Vec::new();
    for row in rows {
        match &row[0] {
            ClausePat::Any => out.push(
                std::iter::repeat_n(ClausePat::Any, arity)
                    .chain(row[1..].iter().cloned())
                    .collect(),
            ),
            ClausePat::Ctor(d, args) => {
                if !c.same_type(d) {
                    return None;
                }
                if c.may_equal(d) {
                    if args.len() != arity {
                        return None;
                    }
                    out.push(args.iter().chain(&row[1..]).cloned().collect());
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The types the reference enumerates. An `Int` has the values `0..4`
    /// and one more no constant spells, standing for the rest of an open
    /// kind.
    #[derive(Debug, Clone)]
    enum Model {
        Unit,
        Bool,
        Int,
        Tuple(Vec<Model>),
        /// A union of cases, each with its fields' types.
        Union(Vec<Vec<Model>>),
    }

    /// A value of a [`Model`] type.
    #[derive(Debug, Clone, PartialEq)]
    enum Value {
        Unit,
        Bool(bool),
        /// `0..4`, or `4` for the value no constant spells.
        Int(u8),
        Tuple(Vec<Value>),
        Case(usize, Vec<Value>),
    }

    /// A pattern with the true value of each constant, which reachability
    /// sees only through its spelling.
    #[derive(Debug, Clone)]
    enum RefPat {
        Any,
        Unit,
        Bool(bool),
        /// The constant's value, and whether it is spelt canonically.
        Int(u8, bool),
        Tuple(Vec<RefPat>),
        Case(usize, Vec<RefPat>),
    }

    fn values(t: &Model) -> Vec<Value> {
        match t {
            Model::Unit => vec![Value::Unit],
            Model::Bool => vec![Value::Bool(false), Value::Bool(true)],
            Model::Int => (0..5).map(Value::Int).collect(),
            Model::Tuple(ts) => product(ts).into_iter().map(Value::Tuple).collect(),
            Model::Union(cases) => cases
                .iter()
                .enumerate()
                .flat_map(|(i, fields)| {
                    product(fields)
                        .into_iter()
                        .map(move |vs| Value::Case(i, vs))
                })
                .collect(),
        }
    }

    fn product(ts: &[Model]) -> Vec<Vec<Value>> {
        ts.iter().fold(vec![Vec::new()], |acc, t| {
            let vs = values(t);
            acc.iter()
                .flat_map(|prefix| {
                    vs.iter().map(move |v| {
                        let mut p = prefix.clone();
                        p.push(v.clone());
                        p
                    })
                })
                .collect()
        })
    }

    fn matches(p: &RefPat, v: &Value) -> bool {
        match (p, v) {
            (RefPat::Any, _) | (RefPat::Unit, Value::Unit) => true,
            (RefPat::Bool(a), Value::Bool(b)) => a == b,
            (RefPat::Int(a, _), Value::Int(b)) => a == b,
            (RefPat::Tuple(ps), Value::Tuple(vs)) => ps.iter().zip(vs).all(|(p, v)| matches(p, v)),
            (RefPat::Case(i, ps), Value::Case(j, vs)) => {
                i == j && ps.iter().zip(vs).all(|(p, v)| matches(p, v))
            }
            _ => false,
        }
    }

    /// Union `0` is the only union a generated case set has, so its name
    /// is fixed.
    fn lower(p: &RefPat, t: &Model) -> ClausePat<u8> {
        match (p, t) {
            (RefPat::Any, _) => ClausePat::Any,
            (RefPat::Unit, _) => ClausePat::Ctor(Ctor::Unit, vec![]),
            (RefPat::Bool(b), _) => ClausePat::Ctor(Ctor::Bool(*b), vec![]),
            (RefPat::Int(v, canonical), _) => ClausePat::Ctor(
                Ctor::Const {
                    kind: ConstKind::Int,
                    value: canonical.then(|| v.to_string().into_boxed_str()),
                },
                vec![],
            ),
            (RefPat::Tuple(ps), Model::Tuple(ts)) => ClausePat::Ctor(
                Ctor::Tuple(ps.len()),
                ps.iter().zip(ts).map(|(p, t)| lower(p, t)).collect(),
            ),
            (RefPat::Case(i, ps), Model::Union(cases)) => ClausePat::Ctor(
                Ctor::Case {
                    union: 0,
                    index: *i,
                    siblings: cases.len(),
                },
                ps.iter()
                    .zip(&cases[*i])
                    .map(|(p, t)| lower(p, t))
                    .collect(),
            ),
            _ => unreachable!("a pattern of another type"),
        }
    }

    fn model() -> impl Strategy<Value = Model> {
        let leaf = prop_oneof![Just(Model::Unit), Just(Model::Bool), Just(Model::Int)];
        leaf.prop_recursive(3, 12, 3, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 2..=3).prop_map(Model::Tuple),
                prop::collection::vec(prop::collection::vec(inner, 0..=2), 1..=3)
                    .prop_map(Model::Union),
            ]
        })
    }

    fn pat(t: &Model) -> BoxedStrategy<RefPat> {
        let wild = Just(RefPat::Any).boxed();
        let ctor = match t {
            Model::Unit => Just(RefPat::Unit).boxed(),
            Model::Bool => any::<bool>().prop_map(RefPat::Bool).boxed(),
            Model::Int => (0u8..4, prop::bool::weighted(0.8))
                .prop_map(|(v, c)| RefPat::Int(v, c))
                .boxed(),
            Model::Tuple(ts) => ts
                .iter()
                .map(pat)
                .collect::<Vec<_>>()
                .prop_map(RefPat::Tuple)
                .boxed(),
            Model::Union(cases) => {
                let cases = cases.clone();
                (0..cases.len())
                    .prop_flat_map(move |i| {
                        cases[i]
                            .iter()
                            .map(pat)
                            .collect::<Vec<_>>()
                            .prop_map(move |ps| RefPat::Case(i, ps))
                    })
                    .boxed()
            }
        };
        prop_oneof![1 => wild, 3 => ctor].boxed()
    }

    fn clauses() -> impl Strategy<Value = (Model, Vec<RefPat>)> {
        model().prop_flat_map(|t| {
            let p = pat(&t);
            (Just(t), prop::collection::vec(p, 1..=5))
        })
    }

    fn reachable_by_enumeration(t: &Model, earlier: &[RefPat], clause: &RefPat) -> bool {
        values(t)
            .iter()
            .any(|v| matches(clause, v) && !earlier.iter().any(|p| matches(p, v)))
    }

    fn canonical(p: &RefPat) -> bool {
        match p {
            RefPat::Int(_, c) => *c,
            RefPat::Tuple(ps) | RefPat::Case(_, ps) => ps.iter().all(canonical),
            RefPat::Any | RefPat::Unit | RefPat::Bool(_) => true,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// A clause judged reachable has a value no earlier clause takes;
        /// and when every constant is spelt canonically, the judgement is
        /// exact.
        #[test]
        fn reachability_agrees_with_enumeration((t, pats) in clauses()) {
            for i in 0..pats.len() {
                let earlier: Vec<ClausePat<u8>> = pats[..i].iter().map(|p| lower(p, &t)).collect();
                let ours = provably_reachable(&earlier, &lower(&pats[i], &t));
                let truth = reachable_by_enumeration(&t, &pats[..i], &pats[i]);
                prop_assert!(!ours || truth, "judged reachable, but every value is taken: {:?} after {:?} at {:?}", pats[i], &pats[..i], t);
                if pats[..=i].iter().all(canonical) {
                    prop_assert_eq!(ours, truth, "{:?} after {:?} at {:?}", pats[i], &pats[..i], t);
                }
            }
        }
    }

    #[test]
    fn a_column_of_two_types_proves_nothing() {
        let int = |v: &str| {
            ClausePat::<u8>::Ctor(
                Ctor::Const {
                    kind: ConstKind::Int,
                    value: Some(v.into()),
                },
                vec![],
            )
        };
        let s = ClausePat::Ctor(
            Ctor::Const {
                kind: ConstKind::Str,
                value: Some("a".into()),
            },
            vec![],
        );
        assert!(provably_reachable(&[int("1")], &int("2")));
        assert!(!provably_reachable(&[int("1")], &s));
        assert!(provably_reachable(&[int("1")], &ClausePat::Any));
    }
}
