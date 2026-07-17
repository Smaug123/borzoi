//! The type representation for semantic analysis (Phase 3 inference).
//!
//! [`Ty`] is a closed, structured description of an F# type — a data value, not
//! a string (per "data descriptions over behavioural abstractions"). Phase 3.1
//! (literal typing) populates only the ground shapes a literal can have:
//! nullary named primitives and `byte[]`; Stage 3.2b-2 adds [`Ty::Tuple`];
//! Stage 3.2c-2b adds [`Ty::Fun`], the curried function type an emitted
//! monomorphic function binder carries. Generic arguments arrive with a later
//! phase (annotation / member typing), extending this DU mechanically.
//!
//! Phase 3.2a adds the **inference variable** [`Ty::Var`], the unknown the
//! unification substrate ([`crate::unify`]) solves for. A `Var` is an *internal*
//! shape: it appears in the terms the solver manipulates, never in
//! [`crate::infer_file`]'s output, which emits only **ground** types
//! ([`Ty::is_ground`]) — the D5 "say nothing when unsure" contract, enforced at
//! read-off by code rather than convention.
//!
//! The canonical rendering ([`Ty::render`]) is the **differential currency**: it
//! emits the abbreviation-resolved BCL FQN convention (`System.Int32`,
//! `System.Byte[]`) that the FCS oracle's `TypeCanon` field also emits
//! (`tools/fcs-dump` `renderTypeCanonical`), so the two sides compare by string
//! equality — the same projection-to-a-shared-form discipline the parser
//! differential uses.

/// An inference variable: a placeholder the unification substrate (the
/// crate-internal `unify` module) solves for. A newtype over the `ena`
/// [`UnifyKey`] index, not a bare `u32`, per "no primitive obsession".
///
/// [`UnifyKey`]: ena::unify::UnifyKey
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TyVid(pub(crate) u32);

impl TyVid {
    /// The underlying table index.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// A best-effort F# type, as far as the current inference phase models it.
///
/// Closed DU: every variant corresponds to an actual type shape, so illegal
/// states (e.g. an array of nothing) are unrepresentable. Stage 3.1 produces
/// only [`Ty::Named`] (nullary) and [`Ty::Array`]; Stage 3.2a adds [`Ty::Var`]
/// for the unification substrate (never emitted from [`crate::infer_file`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    /// A named type by its canonical, abbreviation-resolved dotted path —
    /// `["System", "Int32"]` for `int`, `["System", "String"]` for `string`.
    /// No generic-argument list yet: Stage 3.1 literal typing produces only
    /// nullary primitives, and "no speculative generality" says the `args`
    /// field waits until generic/annotation typing has a use for it.
    Named(Vec<String>),
    /// An array type. `elem` is the element type, `rank` the dimensionality
    /// (1 for `T[]`, 2 for `T[,]`, …). Stage 3.1 produces only `byte[]`.
    Array { elem: Box<Ty>, rank: u32 },
    /// A reference tuple type (`int * string`), the element types in source
    /// order. Always arity ≥ 2 (a 1-element "tuple" is just the element; F# has
    /// no 0-tuple — `unit` is its own type). Stage 3.2b-2.
    Tuple(Vec<Ty>),
    /// A function type `arg -> ret`. Currying is nested to the right, matching
    /// FCS: `a -> b -> c` is `Fun(a, Fun(b, c))`. Stage 3.2c-2b emits it on a
    /// monomorphic function binder (`let f c = if c then 1 else 2` ⇒
    /// `bool -> int`); a polymorphic function stays a [`Ty::Var`] under the
    /// hood and so is never emitted (not [`ground`](Ty::is_ground)) until
    /// generalisation (3.2c-2c).
    Fun { arg: Box<Ty>, ret: Box<Ty> },
    /// A **bound** (quantified) type parameter of a `let`-generalised scheme,
    /// numbered by first appearance (`Param(0)` renders `'a`, `Param(1)` `'b`,
    /// …). Distinct from [`Ty::Var`]: a `Var` is an *unsolved* unknown (not
    /// ground), whereas a `Param` is a *resolved* quantifier standing for "any
    /// type" — so it **is** [`ground`](Ty::is_ground) and reaches read-off.
    /// Introduced at generalisation (Stage 3.2c-2c): a walk-complete, unpoisoned
    /// function binding whose resolved type still has open variables created in
    /// that binding has those variables replaced by `Param`s to form its scheme;
    /// each use instantiates the scheme with fresh [`Ty::Var`]s (see
    /// [`crate::infer_file`]).
    /// A `Param` is never bound *into* the unification table — schemes are
    /// instantiated before their bodies touch the solver — so the unify arms that
    /// treat it as a rigid constant (equal only to the same index) are, in
    /// practice, unreachable; they are kept total and property-tested regardless.
    Param(u32),
    /// An inference variable, solved by the unification substrate. Internal to
    /// inference: it appears in the terms the solver manipulates but never in
    /// [`crate::infer_file`]'s output (which emits only [`ground`](Ty::is_ground)
    /// types).
    Var(TyVid),
}

impl Ty {
    /// A nullary [`Ty::Named`] from a dotted canonical path (`"System.Int32"`).
    pub fn named(path: &str) -> Ty {
        Ty::Named(path.split('.').map(str::to_owned).collect())
    }

    /// Whether this type is fully resolved — contains no [`Ty::Var`]. The
    /// boundary check `infer_file` applies before emitting a type: only ground
    /// types reach a consumer, so an unsolved variable becomes silence (D5)
    /// rather than a wrong or meaningless answer.
    pub fn is_ground(&self) -> bool {
        match self {
            Ty::Named(_) => true,
            Ty::Array { elem, .. } => elem.is_ground(),
            Ty::Tuple(elems) => elems.iter().all(Ty::is_ground),
            Ty::Fun { arg, ret } => arg.is_ground() && ret.is_ground(),
            // A quantified parameter is *resolved* (it stands for "any type"),
            // not an unsolved unknown — so a scheme body containing `Param`s is
            // ground and reaches read-off. Only a [`Ty::Var`] is non-ground.
            Ty::Param(_) => true,
            Ty::Var(_) => false,
        }
    }

    /// Render to the canonical string the FCS oracle's `TypeCanon` emits:
    /// `.`-joined segments for a named type and `Elem[,…]` for an array (rank
    /// commas, as ECMA-335 / FCS render them). The currency the inference
    /// differential (`crates/sema/tests/all/infer_literals_diff.rs`) compares.
    pub fn render(&self) -> String {
        match self {
            Ty::Named(path) => path.join("."),
            Ty::Array { elem, rank } => {
                let commas = ",".repeat((*rank as usize).saturating_sub(1));
                format!("{}[{}]", elem.render(), commas)
            }
            // `a * b * …` with FQN elements, matching the oracle's canonical
            // tuple rendering (`fcs-dump`'s `renderTypeInScope`). A nested tuple
            // element is parenthesised so the flat ` * ` join stays unambiguous.
            Ty::Tuple(elems) => render_tuple(elems, Ty::render),
            // `arg -> ret` with FQN operands, matching the oracle's canonical
            // function rendering (`fcs-dump`'s `renderTypeCanonical`). Right-
            // associative, so only a *function* domain is parenthesised.
            Ty::Fun { arg, ret } => render_fun(arg, ret, Ty::render),
            // A quantified type parameter renders `'a`, `'b`, … by index — the
            // same convention the oracle's canonicaliser
            // (`tools/fcs-dump`'s `renderTypeCanonical`) renames FCS's arbitrary
            // typar names to, so a generalised scheme compares by string equality.
            Ty::Param(i) => typar_name(*i),
            // An unsolved variable has no canonical type; `infer_file` never
            // emits one (it emits only [`ground`](Ty::is_ground) types), so this
            // is reachable only from debugging. Render an anonymous typar to
            // stay total rather than panicking.
            Ty::Var(_) => "'_".to_owned(),
        }
    }

    /// Render to the **F# display form** an editor shows — `int`, `float`,
    /// `byte[]` — rather than the canonical BCL FQN [`Self::render`] emits. The
    /// hover/completion currency; the canonical form stays the oracle currency.
    ///
    /// The BCL-primitive ↔ F#-alias map is **not** restated here: it defers to
    /// [`borzoi_assembly::fsharp_alias`], the single source of truth this
    /// crate already uses to render referenced-assembly members — so a hovered
    /// literal and a hovered member agree (`uint`, not `uint32`). A named type
    /// with no alias falls back to its canonical dotted string, so this is total.
    pub fn render_fsharp(&self) -> String {
        match self {
            Ty::Named(path) => {
                let (namespace, name) = match path.split_last() {
                    Some((name, ns)) => (ns.join("."), name.as_str()),
                    None => return String::new(),
                };
                borzoi_assembly::fsharp_alias(&namespace, name)
                    .map(str::to_owned)
                    .unwrap_or_else(|| path.join("."))
            }
            Ty::Array { elem, rank } => {
                let commas = ",".repeat((*rank as usize).saturating_sub(1));
                format!("{}[{}]", elem.render_fsharp(), commas)
            }
            // The F# display form is the same `a * b` shape, with aliased
            // elements (`int * string`).
            Ty::Tuple(elems) => render_tuple(elems, Ty::render_fsharp),
            // The F# display form is the same `a -> b` shape, with aliased
            // operands (`bool -> int`).
            Ty::Fun { arg, ret } => render_fun(arg, ret, Ty::render_fsharp),
            // A quantified parameter renders identically in the display form —
            // `'a`, `'b`, … — so hover shows `f : 'a -> 'a` for a generalised
            // binder (Stage 3.2c-2c).
            Ty::Param(i) => typar_name(*i),
            // Not emitted from `infer_file` (only ground types are); see
            // [`Self::render`].
            Ty::Var(_) => "'_".to_owned(),
        }
    }
}

/// The canonical name of the type parameter at position `i`: `'a`, `'b`, …,
/// `'z` for the first 26, then a fixed overflow scheme `'t26`, `'t27`, … past
/// `'z`. This is the **shared convention** both sides of the generalisation
/// oracle emit — [`Ty::render`]/[`Ty::render_fsharp`] here, and the
/// `renderTypeCanonical` renamer in `tools/fcs-dump` (which we control, so the
/// scheme is chosen once and mirrored) — so a generalised scheme compares by
/// string equality regardless of FCS's arbitrary internal typar names. The `'t`
/// prefix on the overflow tail keeps it unambiguous against the letter run (no
/// single-letter name is `t26`).
pub(crate) fn typar_name(i: u32) -> String {
    if i < 26 {
        format!("'{}", (b'a' + i as u8) as char)
    } else {
        format!("'t{i}")
    }
}

/// Render a tuple's elements with `render_elem`, joined by ` * `, parenthesising
/// any element that is itself a **tuple** or a **function** so the flat join
/// stays unambiguous: a nested `(a * b) * c` does not collapse into the 3-tuple
/// `a * b * c`, and a function element `(a -> b) * c` does not read as
/// `a -> (b * c)` (since `*` binds tighter than `->`, an unparenthesised function
/// element would be mis-grouped). Shared by [`Ty::render`] and
/// [`Ty::render_fsharp`], and matched by the oracle's `renderTypeCanonical`.
fn render_tuple(elems: &[Ty], render_elem: fn(&Ty) -> String) -> String {
    elems
        .iter()
        .map(|e| match e {
            Ty::Tuple(_) | Ty::Fun { .. } => format!("({})", render_elem(e)),
            _ => render_elem(e),
        })
        .collect::<Vec<_>>()
        .join(" * ")
}

/// Render a function type `arg -> ret` with `render_elem`, matching FCS's
/// canonical/display form. `->` is right-associative and binds looser than `*`,
/// so the *range* is never parenthesised (a curried `a -> b -> c` reads flat)
/// and the *domain* is parenthesised only when it is itself a function
/// (`(a -> b) -> c`); a tuple domain (`a * b -> c`) needs none. Shared by
/// [`Ty::render`] and [`Ty::render_fsharp`].
fn render_fun(arg: &Ty, ret: &Ty, render_elem: fn(&Ty) -> String) -> String {
    let arg_s = match arg {
        Ty::Fun { .. } => format!("({})", render_elem(arg)),
        _ => render_elem(arg),
    };
    format!("{arg_s} -> {}", render_elem(ret))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_named_and_array() {
        assert_eq!(Ty::named("System.Int32").render(), "System.Int32");
        assert_eq!(Ty::named("System.String").render(), "System.String");
        assert_eq!(
            Ty::Array {
                elem: Box::new(Ty::named("System.Byte")),
                rank: 1
            }
            .render(),
            "System.Byte[]"
        );
        assert_eq!(
            Ty::Array {
                elem: Box::new(Ty::named("System.Int32")),
                rank: 2
            }
            .render(),
            "System.Int32[,]"
        );
    }

    #[test]
    fn renders_tuples() {
        let pair = Ty::Tuple(vec![Ty::named("System.Int32"), Ty::named("System.String")]);
        assert_eq!(pair.render(), "System.Int32 * System.String");
        assert_eq!(pair.render_fsharp(), "int * string");

        // A nested tuple element is parenthesised so the join stays unambiguous.
        let nested = Ty::Tuple(vec![
            Ty::Tuple(vec![Ty::named("System.Int32"), Ty::named("System.String")]),
            Ty::named("System.Boolean"),
        ]);
        assert_eq!(
            nested.render(),
            "(System.Int32 * System.String) * System.Boolean"
        );
        assert_eq!(nested.render_fsharp(), "(int * string) * bool");
    }

    #[test]
    fn renders_functions() {
        let fun = |a: Ty, b: Ty| Ty::Fun {
            arg: Box::new(a),
            ret: Box::new(b),
        };

        // A monomorphic function: canonical FQNs and F# aliases.
        let simple = fun(Ty::named("System.Boolean"), Ty::named("System.Int32"));
        assert_eq!(simple.render(), "System.Boolean -> System.Int32");
        assert_eq!(simple.render_fsharp(), "bool -> int");

        // Currying is right-associative — a nested range reads flat, no parens.
        let curried = fun(
            Ty::named("System.Boolean"),
            fun(Ty::named("System.Int32"), Ty::named("System.String")),
        );
        assert_eq!(
            curried.render(),
            "System.Boolean -> System.Int32 -> System.String"
        );
        assert_eq!(curried.render_fsharp(), "bool -> int -> string");

        // A *function* domain is parenthesised; a tuple domain is not (`*`
        // binds tighter than `->`).
        let higher_order = fun(
            fun(Ty::named("System.Int32"), Ty::named("System.Int32")),
            Ty::named("System.Boolean"),
        );
        assert_eq!(
            higher_order.render(),
            "(System.Int32 -> System.Int32) -> System.Boolean"
        );
        assert_eq!(higher_order.render_fsharp(), "(int -> int) -> bool");

        let tuple_domain = fun(
            Ty::Tuple(vec![Ty::named("System.Int32"), Ty::named("System.Int32")]),
            Ty::named("System.Int32"),
        );
        assert_eq!(
            tuple_domain.render(),
            "System.Int32 * System.Int32 -> System.Int32"
        );
        assert_eq!(tuple_domain.render_fsharp(), "int * int -> int");
    }

    #[test]
    fn tuple_with_a_function_element_parenthesises_it() {
        // `(bool -> int) * int`: the function element must be parenthesised so it
        // does not read as `bool -> (int * int)` (`*` binds tighter than `->`).
        let ty = Ty::Tuple(vec![
            Ty::Fun {
                arg: Box::new(Ty::named("System.Boolean")),
                ret: Box::new(Ty::named("System.Int32")),
            },
            Ty::named("System.Int32"),
        ]);
        assert_eq!(
            ty.render(),
            "(System.Boolean -> System.Int32) * System.Int32"
        );
        assert_eq!(ty.render_fsharp(), "(bool -> int) * int");
    }

    #[test]
    fn function_is_ground_only_when_both_operands_are() {
        let ground = Ty::Fun {
            arg: Box::new(Ty::named("System.Boolean")),
            ret: Box::new(Ty::named("System.Int32")),
        };
        assert!(ground.is_ground());

        let open = Ty::Fun {
            arg: Box::new(Ty::Var(TyVid(0))),
            ret: Box::new(Ty::named("System.Int32")),
        };
        assert!(!open.is_ground());
    }

    #[test]
    fn renders_fsharp_primitives() {
        // Every BCL FQN Stage-3.1 literal typing produces maps to the same F#
        // alias the assembly crate uses (note `uint`, not `uint32`).
        let cases = [
            ("System.Int32", "int"),
            ("System.SByte", "sbyte"),
            ("System.Byte", "byte"),
            ("System.Int16", "int16"),
            ("System.UInt16", "uint16"),
            ("System.UInt32", "uint"),
            ("System.Int64", "int64"),
            ("System.UInt64", "uint64"),
            ("System.IntPtr", "nativeint"),
            ("System.UIntPtr", "unativeint"),
            ("System.Double", "float"),
            ("System.Single", "float32"),
            ("System.Decimal", "decimal"),
            ("System.String", "string"),
            ("System.Char", "char"),
            ("System.Boolean", "bool"),
        ];
        for (canonical, fsharp) in cases {
            assert_eq!(Ty::named(canonical).render_fsharp(), fsharp, "{canonical}");
        }
    }

    #[test]
    fn renders_fsharp_byte_array() {
        assert_eq!(
            Ty::Array {
                elem: Box::new(Ty::named("System.Byte")),
                rank: 1
            }
            .render_fsharp(),
            "byte[]"
        );
    }

    #[test]
    fn renders_type_parameters() {
        // The first 26 are `'a`..`'z`; index 26+ overflow to `'t26`, `'t27`, …
        assert_eq!(Ty::Param(0).render(), "'a");
        assert_eq!(Ty::Param(1).render(), "'b");
        assert_eq!(Ty::Param(25).render(), "'z");
        assert_eq!(Ty::Param(26).render(), "'t26");
        assert_eq!(Ty::Param(27).render(), "'t27");
        // The display form is identical.
        assert_eq!(Ty::Param(0).render_fsharp(), "'a");
        assert_eq!(Ty::Param(26).render_fsharp(), "'t26");

        // A quantified parameter is ground (it stands for "any type"), so a
        // scheme body reaches read-off.
        assert!(Ty::Param(0).is_ground());
        let scheme = Ty::Fun {
            arg: Box::new(Ty::Param(0)),
            ret: Box::new(Ty::Param(0)),
        };
        assert!(scheme.is_ground());
        assert_eq!(scheme.render(), "'a -> 'a");
        assert_eq!(scheme.render_fsharp(), "'a -> 'a");
    }

    #[test]
    fn renders_scheme_with_params_in_tuples_and_functions() {
        // `'a -> ('b -> 'b) * 'a` — the function tuple element parenthesised, the
        // curried range flat — matching FCS's canonical form for `let h x = (id, x)`.
        let scheme = Ty::Fun {
            arg: Box::new(Ty::Param(0)),
            ret: Box::new(Ty::Tuple(vec![
                Ty::Fun {
                    arg: Box::new(Ty::Param(1)),
                    ret: Box::new(Ty::Param(1)),
                },
                Ty::Param(0),
            ])),
        };
        assert_eq!(scheme.render(), "'a -> ('b -> 'b) * 'a");
        assert_eq!(scheme.render_fsharp(), "'a -> ('b -> 'b) * 'a");
    }

    #[test]
    fn render_fsharp_falls_back_to_canonical_for_unmapped() {
        // A named type with no F# alias renders by its dotted FQN.
        assert_eq!(
            Ty::named("System.Collections.Generic.List").render_fsharp(),
            "System.Collections.Generic.List"
        );
    }
}
