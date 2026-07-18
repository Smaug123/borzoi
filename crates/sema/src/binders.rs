//! Pure extraction of the binders a pattern introduces.

use crate::def::{Def, DefKind, RANGE_STEP_OP_NAME};
use borzoi_cst::syntax::{AstNode, LongIdentPat, Pat, SyntaxToken};

/// The role a pattern plays where it appears, which fixes the [`DefKind`] of
/// the names it introduces.
///
/// This is necessary because the pattern alone cannot tell you what it binds:
/// the same `Named` pattern `x` is a *value* in `let x = …` but a *parameter*
/// in `fun x -> …`. The caller, which knows the syntactic context, supplies
/// the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinderRole {
    /// The head pattern of a `let` (or module-level) value/function binding.
    Let,
    /// A parameter pattern: a lambda argument, or a function-binding argument.
    Param,
    /// A refutable matching pattern that is not a parameter — a `match`
    /// clause's pattern. Structurally identical to [`BinderRole::Param`]
    /// (constructor heads are references; leaves bind), but its leaf binders
    /// are [`DefKind::PatternLocal`] rather than [`DefKind::Parameter`].
    Pattern,
}

/// The names `pat` introduces, in source order.
///
/// Pure and structural: it recurses through `Paren` / `Typed` (the type
/// annotation contributes no binders) / `Tuple`, and `Wildcard` / `Const` /
/// `Null` bind nothing.
///
/// A `LongIdent` head is read by two orthogonal facts: whether it is *applied*
/// (has argument patterns) and whether it sits at the **direct** head of a
/// `let` binding.
///
/// - **Applied at the direct `let` head** (`let f a b = …`): the
///   function-binding form. The head identifier is the bound function
///   ([`DefKind::Value`] with `is_function = true`) and each argument recurses
///   as a [`BinderRole::Param`].
/// - **Applied anywhere else** (`let (Some x) = …`, `fun (Some x) -> …`): a
///   constructor / active-pattern *application*. The head is a reference, not a
///   binder, so only the arguments bind — as values in a `let` deconstruction,
///   as parameters under [`BinderRole::Param`].
/// - **Nullary, single-segment** (`let Foo = …`, `let (X) = …`, `let X, y = …`,
///   `fun X -> …`): the *maybe-var* reading (FCS's `mkSynPatMaybeVar`). The
///   head binds — a value in `let` positions, a parameter under
///   [`BinderRole::Param`]. Whether the name is *really* a nullary constructor
///   / literal (e.g. `None`) is a resolution question, deferred: such a binder
///   is provisional and Stage C drops it once the name resolves to a
///   constructor.
/// - **Nullary, multi-segment** (`A.B`): a qualified reference; binds nothing.
///
/// The remaining shapes are structural: it recurses through `Paren` / `Typed`
/// (the type annotation contributes no binders) / `Tuple` / `As` (both
/// operands of `p1 as p2` bind, in source order), and `Wildcard` / `Const` /
/// `Null` bind nothing.
pub fn binders(pat: &Pat, role: BinderRole) -> Vec<Def> {
    let mut out = Vec::new();
    collect(pat, Ctx::from_role(role), &mut out);
    out
}

/// The context a sub-pattern is reached in. Richer than the public
/// [`BinderRole`] because a `let` head has two distinct sub-contexts that a
/// caller can never *enter* directly — only reach by descent — so they are
/// not exposed as entry points.
#[derive(Debug, Clone, Copy)]
enum Ctx {
    /// The direct head pattern of a `let` binding, before any structural
    /// descent. Here a `LongIdent` is the function-binding form (or, nullary,
    /// the maybe-var value reading).
    LetHead,
    /// Inside a `let` deconstruction (reached by descending past the head):
    /// leaf binders are values, and a `LongIdent` is a constructor reference.
    LetNested,
    /// A parameter position (lambda / function-binding argument): leaf binders
    /// are parameters, and a `LongIdent` is a constructor reference.
    Param,
    /// A `match`-clause pattern position: structurally like [`Ctx::Param`]
    /// (constructor heads are references; leaves bind), but leaves are
    /// [`DefKind::PatternLocal`].
    Pattern,
}

impl Ctx {
    fn from_role(role: BinderRole) -> Self {
        match role {
            BinderRole::Let => Ctx::LetHead,
            BinderRole::Param => Ctx::Param,
            BinderRole::Pattern => Ctx::Pattern,
        }
    }

    /// The context for a structural child (`Paren` / `Typed` inner, `Tuple`
    /// element). Descending past a `let` head leaves the function-binding
    /// position, so `LetHead` becomes `LetNested`; the others are unchanged.
    fn descend(self) -> Self {
        match self {
            Ctx::LetHead | Ctx::LetNested => Ctx::LetNested,
            Ctx::Param => Ctx::Param,
            Ctx::Pattern => Ctx::Pattern,
        }
    }

    /// The [`DefKind`] for a leaf `Named` binder reached in this context.
    fn leaf_kind(self) -> DefKind {
        match self {
            Ctx::LetHead | Ctx::LetNested => DefKind::Value { is_function: false },
            Ctx::Param => DefKind::Parameter,
            Ctx::Pattern => DefKind::PatternLocal,
        }
    }
}

fn collect(pat: &Pat, ctx: Ctx, out: &mut Vec<Def>) {
    match pat {
        Pat::Named(p) => {
            if let Some(tok) = p.ident() {
                out.push(Def::from_token(&tok, ctx.leaf_kind()));
            } else if let Some(rs) = p.range_step_op() {
                // Nullary range-step operator binding (`let (.. ..) = …`): its name
                // is a `RANGE_STEP_OP` node, keyed on the canonical `.. ..` so a
                // reference of any layout resolves to it.
                out.push(Def::from_op_name(
                    RANGE_STEP_OP_NAME,
                    rs.syntax().text_range(),
                    ctx.leaf_kind(),
                ));
            }
        }
        Pat::OptionalVal(p) => {
            // `?x` — the optional-argument pattern. It introduces a single name
            // binding exactly like `Named` (the `?` only marks the parameter
            // optional); the bound ident is `x`, backticks already stripped by
            // the token text.
            if let Some(tok) = p.ident() {
                out.push(Def::from_token(&tok, ctx.leaf_kind()));
            }
        }
        // Refutable heads that introduce no names. `IsInst` (`:? T`) tests a
        // type and binds nothing on its own; the `x` in `:? T as x` is bound by
        // the surrounding `As`, not the `IsInst`. `Quote` (`<@ … @>`) is a
        // parameterised active-pattern *argument* expression, not a binder.
        Pat::Wildcard(_) | Pat::Const(_) | Pat::Null(_) | Pat::IsInst(_) | Pat::Quote(_) => {}
        Pat::Paren(p) => {
            if let Some(inner) = p.inner() {
                collect(&inner, ctx.descend(), out);
            }
        }
        Pat::Typed(p) => {
            // The annotation (`p.ty()`) names types, never binders.
            if let Some(inner) = p.pat() {
                collect(&inner, ctx.descend(), out);
            }
        }
        Pat::Attrib(p) => {
            // `[<Foo>] p` — the attribute lists name attribute *types*, never
            // binders; only the inner pattern binds. Transparent, exactly like
            // `Typed`.
            if let Some(inner) = p.pat() {
                collect(&inner, ctx.descend(), out);
            }
        }
        Pat::Tuple(p) => {
            for el in p.elements() {
                collect(&el, ctx.descend(), out);
            }
        }
        Pat::ArrayOrList(p) => {
            // A list `[x; y]` / array `[| x; y |]` pattern binds each element's
            // names, exactly like a `Tuple` element — value deconstruction, never
            // a function-binding head, so every element descends the context.
            for el in p.elements() {
                collect(&el, ctx.descend(), out);
            }
        }
        Pat::Record(p) => {
            // A record pattern `{ X = p; … }` binds the names in each field's
            // *value* pattern; the field name (`X`) references a record field,
            // not a binder. Value deconstruction, so each value descends the
            // context — exactly like a `Tuple` element.
            for field in p.fields() {
                if let Some(value) = field.pat() {
                    collect(&value, ctx.descend(), out);
                }
            }
        }
        Pat::As(p) => {
            // `p1 as p2` binds the names of both operands, in source order
            // (lhs then rhs). An `as`-pattern is never a function-binding head,
            // so both operands descend the context, exactly like a `Tuple`
            // element — a `let`-head `as` is a value deconstruction.
            if let Some(lhs) = p.lhs() {
                collect(&lhs, ctx.descend(), out);
            }
            if let Some(rhs) = p.rhs() {
                collect(&rhs, ctx.descend(), out);
            }
        }
        Pat::ListCons(p) => {
            // `h :: t` binds the names of both operands, in source order. A cons
            // pattern is value deconstruction, never a function-binding head, so
            // both operands descend the context — exactly like a `Tuple` element
            // or an `As`.
            if let Some(lhs) = p.lhs() {
                collect(&lhs, ctx.descend(), out);
            }
            if let Some(rhs) = p.rhs() {
                collect(&rhs, ctx.descend(), out);
            }
        }
        Pat::Ands(p) => {
            // `a & b & c` binds the names of every operand, in source order — a
            // conjunction matches the same value against each operand. Value
            // deconstruction, never a function-binding head, so each operand
            // descends the context, exactly like a `Tuple` element.
            for operand in p.operands() {
                collect(&operand, ctx.descend(), out);
            }
        }
        Pat::Or(p) => {
            // `A | B` binds the same names on both branches (F# requires the two
            // sides to agree). We collect from both — each branch's binder is a
            // genuine binding-occurrence token (both should be found by
            // go-to-def / rename), and the scope frame's last-wins lookup
            // tolerates the duplicate. Precise one-logical-binding unification
            // across the branches is a deeper sema concern, not modelled here.
            if let Some(lhs) = p.lhs() {
                collect(&lhs, ctx.descend(), out);
            }
            if let Some(rhs) = p.rhs() {
                collect(&rhs, ctx.descend(), out);
            }
        }
        Pat::LongIdent(p) => {
            // `arg_pats` spans both `SynArgPats` shapes: the curried list
            // (`p.args()`) and the named-field group `Case (field = pat; …)`
            // (`p.name_pat_pairs()`), whose value patterns bind exactly as
            // curried args do. The two are mutually exclusive, so this is just a
            // concatenation.
            let args = arg_pats(p);
            // A named-field group marks the head as *applied* even when every
            // recovered pair lost its value pattern (`Case (field = )` mid-edit),
            // where `arg_pats` is empty: the `name_pat_pairs` node itself means
            // the head is a constructor / function reference, never a nullary
            // maybe-var binder.
            let has_args = !args.is_empty() || p.name_pat_pairs().is_some();
            match ctx {
                Ctx::LetHead => {
                    // Direct `let` head: `let f a b = …` names the function and
                    // binds its arguments as parameters; `let Foo = …` (nullary)
                    // is the maybe-var value reading. The head binds either way.
                    // Take the last segment for faithfulness to the (future)
                    // `let M.f` member-augmentation form — except on an
                    // active-pattern path (`let A.(|Foo|_|) x = …`), whose
                    // `LONG_IDENT` segments are all *qualifiers*: its last one
                    // (`A`) names a module, not the value being defined (that is
                    // the `ACTIVE_PAT_NAME`, which no head arm binds today).
                    let kind = DefKind::Value {
                        is_function: has_args,
                    };
                    if let Some(head) = p.head().filter(|_| !names_active_pat(p)) {
                        if let Some(rs) = head.range_step_op() {
                            // Applied range-step operator head (`let (.. ..) a b =
                            // …`): the operator name is a `RANGE_STEP_OP` node, not
                            // an `idents()` segment — bind it under the canonical
                            // `.. ..`.
                            out.push(Def::from_op_name(
                                RANGE_STEP_OP_NAME,
                                rs.syntax().text_range(),
                                kind,
                            ));
                        } else if let Some(name) = head.idents().last() {
                            out.push(Def::from_token(&name, kind));
                        }
                    }
                    for arg in &args {
                        collect(arg, Ctx::Param, out);
                    }
                }
                Ctx::LetNested | Ctx::Param | Ctx::Pattern if has_args => {
                    // Constructor / active pattern reached by descent: the head
                    // is a reference, not a binder, so only the arguments bind —
                    // in the same context the application was reached in.
                    for arg in &args {
                        collect(arg, ctx, out);
                    }
                }
                Ctx::LetNested | Ctx::Param | Ctx::Pattern => {
                    // Nullary head reached by descent. A single-segment name is
                    // a maybe-var binder (FCS `mkSynPatMaybeVar`); it binds
                    // provisionally and resolution drops it if the name is
                    // really a nullary constructor / literal. A multi-segment
                    // head (`A.B`) is a qualified reference and binds nothing.
                    if let Some(name) = single_segment(p) {
                        out.push(Def::provisional_from_token(&name, ctx.leaf_kind()));
                    }
                }
            }
        }
    }
}

/// The argument value patterns of a [`LongIdentPat`], spanning both FCS
/// `SynArgPats` shapes: the curried list (`p.args()`, `SynArgPats.Pats`) and the
/// named-field group `Case (field = pat; …)` (`p.name_pat_pairs()`,
/// `SynArgPats.NamePatPairs`). The named form's value patterns bind exactly as
/// curried args do — the field *names* reference union-case fields, not binders,
/// so they are dropped here. The two shapes are mutually exclusive in the
/// grammar, so this is a plain concatenation (one side is always empty).
fn arg_pats(p: &LongIdentPat) -> Vec<Pat> {
    let mut args: Vec<Pat> = p.args().collect();
    if let Some(group) = p.name_pat_pairs() {
        args.extend(group.pairs().filter_map(|pair| pair.pat()));
    }
    args
}

/// The sole identifier token of a single-segment [`LongIdentPat`] head, or
/// `None` when the head is absent, a multi-segment (qualified) path, or the
/// qualifier of an active-pattern path (see [`names_active_pat`]).
fn single_segment(p: &LongIdentPat) -> Option<SyntaxToken> {
    let head = p.head().filter(|_| !names_active_pat(p))?;
    let mut idents = head.idents();
    let first = idents.next()?;
    idents.next().is_none().then_some(first)
}

/// `true` when the path's final segment is an *active-pattern name*
/// (`A.(|Foo|Bar|)`, `M.N.(|Parse|_|)` — FCS's `pathOp` ending in an `opName`).
/// The name is a sibling `ACTIVE_PAT_NAME` node, so the head `LONG_IDENT` holds
/// only the *qualifier* segments — which name a module/type, never a binder. That
/// matters most when there is exactly one of them (`A.(|Foo|Bar|)`): it would
/// otherwise read as a nullary maybe-var head and bind `A` provisionally, which
/// resolution then resolves to any in-scope union case of that name — a wrong
/// target. (An *operator*-terminated path keeps its `( op )` tokens inside the
/// `LONG_IDENT`, so its last segment is the operator itself and the existing
/// last-segment reading stays correct.)
fn names_active_pat(p: &LongIdentPat) -> bool {
    p.active_pat_name().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{AstNode, ImplFile, ModuleDecl};
    use proptest::prelude::*;

    /// Parse `let <pat_src> = 0` and return the binding's head pattern.
    /// Panics on any parse error so a generator that strays outside the
    /// supported subset fails loudly rather than silently testing nothing.
    fn head_pat(pat_src: &str) -> Pat {
        let src = format!("let {pat_src} = 0\n");
        let parse = parse(&src);
        assert!(
            parse.errors.is_empty(),
            "unexpected parse errors for {src:?}: {:?}",
            parse.errors
        );
        let file = ImplFile::cast(parse.root).expect("root is an impl file");
        let module = file.modules().next().expect("one module");
        let decl = module.decls().next().expect("one decl");
        let ModuleDecl::Let(let_decl) = decl else {
            panic!("expected a let decl");
        };
        let binding = let_decl.bindings().next().expect("one binding");
        binding.pat().expect("binding has a head pattern")
    }

    /// Parse arbitrary `source` *tolerating parse errors* (for mid-edit
    /// recovery cases) and return the first `LONG_IDENT_PAT` in the tree as a
    /// [`Pat`]. Preorder, so a clause pattern is found before any nested one.
    fn first_long_ident_pat(source: &str) -> Pat {
        use borzoi_cst::syntax::SyntaxKind;
        let parse = parse(source);
        let node = parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::LONG_IDENT_PAT)
            .expect("a LONG_IDENT_PAT in the tree");
        Pat::cast(node).expect("cast LONG_IDENT_PAT to Pat")
    }

    fn names(defs: &[Def]) -> Vec<String> {
        defs.iter().map(|d| d.name.clone()).collect()
    }

    #[test]
    fn named_value() {
        let defs = binders(&head_pat("x"), BinderRole::Let);
        assert_eq!(names(&defs), ["x"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: false });
    }

    #[test]
    fn named_as_parameter() {
        let defs = binders(&head_pat("x"), BinderRole::Param);
        assert_eq!(names(&defs), ["x"]);
        assert_eq!(defs[0].kind, DefKind::Parameter);
    }

    #[test]
    fn wildcard_const_null_bind_nothing() {
        assert!(binders(&head_pat("_"), BinderRole::Let).is_empty());
        assert!(binders(&head_pat("0"), BinderRole::Let).is_empty());
        assert!(binders(&head_pat("()"), BinderRole::Let).is_empty());
        assert!(binders(&head_pat("null"), BinderRole::Let).is_empty());
    }

    #[test]
    fn function_form_head_is_function_args_are_params() {
        let defs = binders(&head_pat("f x y z"), BinderRole::Let);
        assert_eq!(names(&defs), ["f", "x", "y", "z"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: true });
        assert!(defs[1..].iter().all(|d| d.kind == DefKind::Parameter));
    }

    #[test]
    fn function_form_unit_arg_binds_nothing_but_is_function() {
        let defs = binders(&head_pat("f ()"), BinderRole::Let);
        assert_eq!(names(&defs), ["f"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: true });
    }

    #[test]
    fn function_form_wildcard_args() {
        let defs = binders(&head_pat("f x _ y"), BinderRole::Let);
        assert_eq!(names(&defs), ["f", "x", "y"]);
    }

    #[test]
    fn paren_is_transparent() {
        assert_eq!(names(&binders(&head_pat("(x)"), BinderRole::Let)), ["x"]);
    }

    #[test]
    fn typed_is_transparent_and_drops_the_annotation() {
        // The `int` in the annotation must not appear as a binder.
        let defs = binders(&head_pat("(x : int)"), BinderRole::Let);
        assert_eq!(names(&defs), ["x"]);
    }

    #[test]
    fn typed_wildcard_binds_nothing() {
        assert!(binders(&head_pat("(_ : int)"), BinderRole::Let).is_empty());
    }

    #[test]
    fn tuple_is_concatenation_in_source_order() {
        assert_eq!(
            names(&binders(&head_pat("(x, y, z)"), BinderRole::Let)),
            ["x", "y", "z"]
        );
        // Unparenthesised tuple head.
        assert_eq!(
            names(&binders(&head_pat("a, b"), BinderRole::Let)),
            ["a", "b"]
        );
    }

    #[test]
    fn function_form_with_typed_arg() {
        let defs = binders(&head_pat("f (x : int)"), BinderRole::Let);
        assert_eq!(names(&defs), ["f", "x"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: true });
        assert_eq!(defs[1].kind, DefKind::Parameter);
    }

    #[test]
    fn top_level_nullary_uppercase_head_binds_as_value() {
        // `let Foo = …` is the maybe-var reading: at a *direct* let head a
        // nullary upper-case identifier binds a value. (Whether `Foo` is
        // really a nullary constructor is a resolution question deferred to
        // Stage C.)
        let defs = binders(&head_pat("Foo"), BinderRole::Let);
        assert_eq!(names(&defs), ["Foo"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: false });
    }

    #[test]
    fn let_paren_constructor_binds_only_arg_as_value() {
        // `let (Some x) = …` deconstructs: `Some` is a constructor *reference*,
        // not a new function, so only `x` binds — and as a let value, not a
        // parameter.
        let defs = binders(&head_pat("(Some x)"), BinderRole::Let);
        assert_eq!(names(&defs), ["x"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: false });
    }

    #[test]
    fn let_tuple_with_constructor_binds_values_not_the_constructor() {
        let defs = binders(&head_pat("(x, Some y)"), BinderRole::Let);
        assert_eq!(names(&defs), ["x", "y"]);
        assert!(
            defs.iter()
                .all(|d| d.kind == DefKind::Value { is_function: false })
        );
    }

    #[test]
    fn record_pattern_binds_field_values_not_field_names() {
        // `{ X = a; M.Y = b }` binds the field *values* (`a`, `b`) in source
        // order; the field names (`X`, `M.Y`) reference record fields, not
        // binders. Value deconstruction, so each binds as a value.
        let defs = binders(&head_pat("{ X = a; M.Y = b }"), BinderRole::Let);
        assert_eq!(names(&defs), ["a", "b"]);
        assert!(
            defs.iter()
                .all(|d| d.kind == DefKind::Value { is_function: false })
        );
    }

    #[test]
    fn let_head_name_pat_pairs_binds_function_and_field_values() {
        // `let f (a = x; b = y) = …` is a function-form head whose single
        // argument is a named-field group (FCS's `SynArgPats.NamePatPairs`).
        // `f` binds as a function; the field *values* (`x`, `y`) bind as
        // parameters in source order; the field *names* (`a`, `b`) reference
        // union-case fields, not binders.
        let defs = binders(&head_pat("f (a = x; b = y)"), BinderRole::Let);
        assert_eq!(names(&defs), ["f", "x", "y"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: true });
        assert!(defs[1..].iter().all(|d| d.kind == DefKind::Parameter));
    }

    #[test]
    fn let_deconstruct_name_pat_pairs_binds_field_values_not_constructor() {
        // `let (Case (field = x)) = …` deconstructs: `Case` is a constructor
        // *reference*, not a binder, so only the named field's value `x` binds —
        // as a let value, not a parameter (descent context).
        let defs = binders(&head_pat("(Case (field = x))"), BinderRole::Let);
        assert_eq!(names(&defs), ["x"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: false });
    }

    #[test]
    fn pattern_role_name_pat_pairs_binds_field_values() {
        // In `match`-clause (pattern) position the same group binds the field
        // values as pattern-locals; the constructor head and field names bind
        // nothing.
        let defs = binders(&head_pat("(Case (field = x))"), BinderRole::Pattern);
        assert_eq!(names(&defs), ["x"]);
    }

    #[test]
    fn incomplete_name_pat_pairs_head_is_not_a_maybe_var() {
        // `Case (field = )` mid-edit: the recovered pair has no value pattern,
        // so `arg_pats` is empty — but the named-field group still marks `Case`
        // as an applied constructor reference, not a nullary maybe-var binder.
        // Nothing binds (the head is a reference; the field name and the absent
        // value bind nothing). Without the `name_pat_pairs().is_some()` guard,
        // `Case` would wrongly bind as a provisional local.
        let pat = first_long_ident_pat("match q with Case (field = ) -> 1\n");
        assert!(
            binders(&pat, BinderRole::Pattern).is_empty(),
            "an incomplete named-field constructor pattern binds nothing, got {:?}",
            names(&binders(&pat, BinderRole::Pattern)),
        );
    }

    #[test]
    fn let_nested_nullary_uppercase_is_a_maybe_var_value() {
        // A nullary upper-case head reached by descent is a maybe-var binder,
        // not (yet) a constructor reference: `None` here binds provisionally as
        // a value, to be dropped by resolution if it really is a constructor.
        let defs = binders(&head_pat("(x, None)"), BinderRole::Let);
        assert_eq!(names(&defs), ["x", "None"]);
        assert!(
            defs.iter()
                .all(|d| d.kind == DefKind::Value { is_function: false })
        );
    }

    #[test]
    fn let_paren_nullary_uppercase_binds_like_the_unparenthesised_form() {
        // Parens are transparent: `let (X) = …` binds `X` exactly as
        // `let X = …` does.
        let defs = binders(&head_pat("(X)"), BinderRole::Let);
        assert_eq!(names(&defs), ["X"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: false });
    }

    #[test]
    fn let_tuple_nullary_uppercase_binds_as_value() {
        let defs = binders(&head_pat("X, y"), BinderRole::Let);
        assert_eq!(names(&defs), ["X", "y"]);
        assert!(
            defs.iter()
                .all(|d| d.kind == DefKind::Value { is_function: false })
        );
    }

    #[test]
    fn nullary_uppercase_as_parameter_is_a_maybe_var() {
        // `fun X -> …`: an upper-case lambda parameter binds (maybe-var), as a
        // parameter.
        let defs = binders(&head_pat("X"), BinderRole::Param);
        assert_eq!(names(&defs), ["X"]);
        assert_eq!(defs[0].kind, DefKind::Parameter);
    }

    #[test]
    fn function_param_constructor_pattern_binds_args_as_parameters() {
        // `let f (Some x) = …`: `f` is the function, `Some` a constructor
        // reference, and `x` a *parameter* of `f`.
        let defs = binders(&head_pat("f (Some x)"), BinderRole::Let);
        assert_eq!(names(&defs), ["f", "x"]);
        assert_eq!(defs[0].kind, DefKind::Value { is_function: true });
        assert_eq!(defs[1].kind, DefKind::Parameter);
    }

    #[test]
    fn as_pattern_binds_both_operands_in_source_order() {
        // `let x as y = …` binds both names, lhs first, both as let values.
        let defs = binders(&head_pat("x as y"), BinderRole::Let);
        assert_eq!(names(&defs), ["x", "y"]);
        assert!(
            defs.iter()
                .all(|d| d.kind == DefKind::Value { is_function: false })
        );
    }

    #[test]
    fn as_pattern_with_constructor_lhs_binds_arg_and_alias() {
        // `let (Some x) as y = …`: `Some` is a constructor reference, so the
        // lhs binds only `x`; the rhs binds the alias `y`. Both are values.
        let defs = binders(&head_pat("(Some x) as y"), BinderRole::Let);
        assert_eq!(names(&defs), ["x", "y"]);
        assert!(
            defs.iter()
                .all(|d| d.kind == DefKind::Value { is_function: false })
        );
    }

    #[test]
    fn as_pattern_over_tuple_binds_elements_then_alias() {
        // `let (x, y) as z = …` binds the tuple elements then the whole-value
        // alias, all as values.
        let defs = binders(&head_pat("(x, y) as z"), BinderRole::Let);
        assert_eq!(names(&defs), ["x", "y", "z"]);
        assert!(
            defs.iter()
                .all(|d| d.kind == DefKind::Value { is_function: false })
        );
    }

    #[test]
    fn left_nested_as_chain_binds_in_source_order() {
        // `x as y as z` parses left-associatively as `As(As(x, y), z)`; all
        // three names bind in source order.
        let defs = binders(&head_pat("x as y as z"), BinderRole::Let);
        assert_eq!(names(&defs), ["x", "y", "z"]);
    }

    #[test]
    fn as_pattern_in_parameter_position_binds_parameters() {
        // Under `BinderRole::Param`, both operands of an `as` bind as
        // parameters.
        let defs = binders(&head_pat("(x as y)"), BinderRole::Param);
        assert_eq!(names(&defs), ["x", "y"]);
        assert!(defs.iter().all(|d| d.kind == DefKind::Parameter));
    }

    #[test]
    fn pattern_role_binds_leaves_as_pattern_locals() {
        // Under `BinderRole::Pattern` (a `match` clause), a plain ident binds
        // as a `PatternLocal`, not a value or a parameter.
        let defs = binders(&head_pat("x"), BinderRole::Pattern);
        assert_eq!(names(&defs), ["x"]);
        assert_eq!(defs[0].kind, DefKind::PatternLocal);
    }

    #[test]
    fn pattern_role_constructor_binds_only_args_as_pattern_locals() {
        // `match … with Some x -> …`: `Some` is a constructor reference, `x`
        // a clause-local binder. Structurally identical to the `Param` case
        // but the leaf kind is `PatternLocal`.
        let defs = binders(&head_pat("(Some x)"), BinderRole::Pattern);
        assert_eq!(names(&defs), ["x"]);
        assert_eq!(defs[0].kind, DefKind::PatternLocal);
    }

    #[test]
    fn pattern_role_tuple_binds_each_element_as_pattern_local() {
        let defs = binders(&head_pat("(a, b)"), BinderRole::Pattern);
        assert_eq!(names(&defs), ["a", "b"]);
        assert!(defs.iter().all(|d| d.kind == DefKind::PatternLocal));
    }

    #[test]
    fn nested_nullary_uppercase_head_is_flagged_provisional() {
        // A nullary upper-case head reached by descent (`None` in `(x, None)`)
        // is a maybe-var: flagged provisional so the resolver can drop it if it
        // resolves to a constructor. The definite leaf `x` is not provisional.
        let defs = binders(&head_pat("(x, None)"), BinderRole::Let);
        assert_eq!(names(&defs), ["x", "None"]);
        assert!(!defs[0].provisional, "`x` is a definite binder");
        assert!(defs[1].provisional, "`None` is a provisional maybe-var");
    }

    #[test]
    fn pattern_role_nullary_uppercase_head_is_provisional() {
        let defs = binders(&head_pat("None"), BinderRole::Pattern);
        assert_eq!(names(&defs), ["None"]);
        assert!(defs[0].provisional);
    }

    #[test]
    fn definite_binders_are_not_provisional() {
        // Direct `let` heads (value, function, nullary upper-case head) and
        // plain `Named` leaves are all definite, never provisional.
        for (src, role) in [
            ("x", BinderRole::Let),
            ("Foo", BinderRole::Let),
            ("f x", BinderRole::Let),
            ("x", BinderRole::Param),
            ("(a, b)", BinderRole::Let),
        ] {
            let defs = binders(&head_pat(src), role);
            assert!(
                defs.iter().all(|d| !d.provisional),
                "{src:?} produced a provisional binder: {defs:?}"
            );
        }
    }

    // ---- property test ----------------------------------------------------
    //
    // Generate a *value* pattern (the structural recursion: leaves +
    // paren/typed/tuple), render it to source with fresh distinct idents, and
    // record the binders we expect. `binders` must reproduce exactly that
    // list, every range must slice back to the recorded name, and every leaf
    // must be a plain value.

    #[derive(Debug, Clone)]
    enum Shape {
        Ident,
        Wild,
        Const,
        Paren(Box<Shape>),
        /// `(<atom> : int)` — F# requires the parens; the inner is restricted
        /// to an atom (ident/wildcard) to stay within the parseable subset.
        TypedAtom(Box<Shape>),
        Tuple(Vec<Shape>),
        /// A constructor / active-pattern application `C arg…` (the name is an
        /// upper-case reference, never a binder). Only generated *nested*, so
        /// it is always a deconstruction rather than a function-binding head.
        Ctor(Vec<Shape>),
    }

    fn atom_strategy() -> impl Strategy<Value = Shape> {
        prop_oneof![Just(Shape::Ident), Just(Shape::Wild)]
    }

    fn shape_strategy() -> impl Strategy<Value = Shape> {
        let leaf = prop_oneof![Just(Shape::Ident), Just(Shape::Wild), Just(Shape::Const)];
        leaf.prop_recursive(4, 24, 3, |inner| {
            prop_oneof![
                inner.clone().prop_map(|s| Shape::Paren(Box::new(s))),
                atom_strategy().prop_map(|s| Shape::TypedAtom(Box::new(s))),
                prop::collection::vec(inner, 2..=3).prop_map(Shape::Tuple),
            ]
        })
    }

    /// Render `shape` to F# pattern source, appending each binder's fresh name
    /// (in source order) to `expected`.
    fn render(shape: &Shape, counter: &mut u32, expected: &mut Vec<String>, out: &mut String) {
        match shape {
            Shape::Ident => {
                let name = format!("x{counter}");
                *counter += 1;
                out.push_str(&name);
                expected.push(name);
            }
            Shape::Wild => out.push('_'),
            Shape::Const => out.push('0'),
            Shape::Paren(inner) => {
                out.push('(');
                render(inner, counter, expected, out);
                out.push(')');
            }
            Shape::TypedAtom(inner) => {
                out.push('(');
                render(inner, counter, expected, out);
                out.push_str(" : int)");
            }
            Shape::Tuple(elems) => {
                out.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    render(e, counter, expected, out);
                }
                out.push(')');
            }
            Shape::Ctor(args) => {
                let name = format!("C{counter}");
                *counter += 1;
                out.push_str(&name);
                if args.is_empty() {
                    // Nullary upper-case head: a maybe-var binder.
                    expected.push(name);
                } else {
                    // Applied: the head is a constructor reference, not a
                    // binder; only the arguments bind.
                    for arg in args {
                        out.push(' ');
                        // A constructor argument must be atomic; the only
                        // sub-shape that renders non-atomically is an applied
                        // constructor, so wrap that one in parens.
                        let needs_parens = matches!(arg, Shape::Ctor(a) if !a.is_empty());
                        if needs_parens {
                            out.push('(');
                        }
                        render(arg, counter, expected, out);
                        if needs_parens {
                            out.push(')');
                        }
                    }
                }
            }
        }
    }

    proptest! {
        #[test]
        fn binders_reproduce_generated_value_pattern(shape in shape_strategy()) {
            let mut counter = 0;
            let mut expected = Vec::new();
            let mut pat_src = String::new();
            render(&shape, &mut counter, &mut expected, &mut pat_src);

            let src = format!("let {pat_src} = 0\n");
            let parsed = parse(&src);
            prop_assert!(
                parsed.errors.is_empty(),
                "parse errors for {src:?}: {:?}",
                parsed.errors
            );

            let file = ImplFile::cast(parsed.root).expect("impl file");
            let module = file.modules().next().expect("module");
            let ModuleDecl::Let(let_decl) =
                module.decls().next().expect("decl") else { unreachable!() };
            let pat = let_decl
                .bindings()
                .next()
                .expect("binding")
                .pat()
                .expect("head pat");

            let defs = binders(&pat, BinderRole::Let);

            prop_assert_eq!(names(&defs), expected);
            for d in &defs {
                let span = usize::from(d.range.start())..usize::from(d.range.end());
                prop_assert_eq!(&src[span], d.name.as_str());
                prop_assert_eq!(d.kind, DefKind::Value { is_function: false });
            }
        }
    }

    // ---- property test: let deconstructions with constructors -------------
    //
    // Like the value-pattern property, but the generated pattern may contain
    // constructor / active-pattern applications anywhere *below* a forced
    // container head — so no constructor is ever a function-binding head.
    // `binders` must bind exactly the value leaves *and the nullary maybe-var
    // heads* (in source order, every range slicing back to its name), and never
    // an *applied* constructor's head.

    /// Constructor arguments are restricted to the shapes the parser accepts
    /// in *any* curried-argument position: atoms (`x` / `_` / `0`), a typed
    /// atom (`(x : int)`), or a nullary constructor (`None`). A plain
    /// parenthesised / tuple argument only parses as the *final* argument, so
    /// it is excluded here; deeper constructor nesting is reached instead
    /// through the `Paren` / `Tuple` *containers* in `decon_shape_strategy`.
    fn ctor_arg_strategy() -> impl Strategy<Value = Shape> {
        prop_oneof![
            Just(Shape::Ident),
            Just(Shape::Wild),
            Just(Shape::Const),
            atom_strategy().prop_map(|s| Shape::TypedAtom(Box::new(s))),
            Just(Shape::Ctor(Vec::new())),
        ]
    }

    fn decon_shape_strategy() -> impl Strategy<Value = Shape> {
        let leaf = prop_oneof![Just(Shape::Ident), Just(Shape::Wild), Just(Shape::Const)];
        let inner = leaf.prop_recursive(4, 32, 3, |inner| {
            prop_oneof![
                inner.clone().prop_map(|s| Shape::Paren(Box::new(s))),
                atom_strategy().prop_map(|s| Shape::TypedAtom(Box::new(s))),
                prop::collection::vec(inner, 2..=3).prop_map(Shape::Tuple),
                prop::collection::vec(ctor_arg_strategy(), 0..=3).prop_map(Shape::Ctor),
            ]
        });
        // Force the head to be a container, so any constructor inside is a
        // deconstruction reference rather than a function-binding head.
        prop_oneof![
            inner.clone().prop_map(|s| Shape::Paren(Box::new(s))),
            prop::collection::vec(inner, 2..=3).prop_map(Shape::Tuple),
        ]
    }

    proptest! {
        #[test]
        fn binders_over_let_deconstructions_with_constructors(
            shape in decon_shape_strategy()
        ) {
            let mut counter = 0;
            let mut expected = Vec::new();
            let mut pat_src = String::new();
            render(&shape, &mut counter, &mut expected, &mut pat_src);

            let src = format!("let {pat_src} = 0\n");
            let parsed = parse(&src);
            prop_assert!(
                parsed.errors.is_empty(),
                "parse errors for {src:?}: {:?}",
                parsed.errors
            );

            let file = ImplFile::cast(parsed.root).expect("impl file");
            let module = file.modules().next().expect("module");
            let ModuleDecl::Let(let_decl) =
                module.decls().next().expect("decl") else { unreachable!() };
            let pat = let_decl
                .bindings()
                .next()
                .expect("binding")
                .pat()
                .expect("head pat");

            let defs = binders(&pat, BinderRole::Let);

            prop_assert_eq!(names(&defs), expected);
            for d in &defs {
                let span = usize::from(d.range.start())..usize::from(d.range.end());
                prop_assert_eq!(&src[span], d.name.as_str());
                prop_assert_eq!(d.kind, DefKind::Value { is_function: false });
            }
        }
    }
}
