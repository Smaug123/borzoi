//! Applications of FSharp.Core's statically resolved operators and conversion
//! functions, typed by the rule F# builds in.

use super::*;

impl<'a> Gen<'a> {
    /// The application of `func` to `arg` when it is a use of one of
    /// FSharp.Core's operators or conversion functions whose solution F#
    /// builds in ([`builtin_operator`]), as [`Self::infer_app`] returns it;
    /// `None` when it is not one. Three shapes are: `a OP b` with a binary
    /// one (`+`, `&&&`, `<<<`), and a unary one applied to its operand by name
    /// (`int a`, `Operators.int a`) or as a prefix operator (`-a`). Any other
    /// use — the operator as a value (`(+) a b`, `List.map int`, `a |> int`)
    /// — is not, and declines as a constrained signature
    /// ([`Self::nonlocal_value_use`]).
    ///
    /// FCS solves the operator's constraint where it checks the application,
    /// from its operands' types as it knows them there; one it does not know
    /// yet leaves the constraint to be solved later, by weak resolution if
    /// nothing else fixes it, which may default it to `int`
    /// (`IsBinaryOpOtherArgType`). So the operands are walked as the
    /// arguments they are, the binding settled ([`Self::settle`], which
    /// applies only what FCS has applied by this point in source order), and
    /// the operand types read there: the application is typed when every one
    /// is known and on the rule's list ([`builtin_verdict`]), and otherwise
    /// the binding is incomplete with the reason and the application untyped —
    /// but for a conversion whose operand is not known, or not modelled,
    /// whose result its signature fixes.
    pub(super) fn builtin_application(
        &mut self,
        scope: &mut Scope,
        func: &Expr,
        arg: &Expr,
    ) -> Option<Option<TyVid>> {
        let (builtin, operands) = match func {
            Expr::App(inner) if inner.is_infix() => {
                let (op, lhs) = (inner.func()?, inner.arg()?);
                let builtin = self.builtin_callee(&op)?;
                if !matches!(
                    builtin,
                    Builtin::Arith(_) | Builtin::Bitwise | Builtin::Shift
                ) {
                    return None;
                }
                (builtin, vec![lhs, arg.clone()])
            }
            Expr::Ident(_) | Expr::LongIdent(_) => {
                let builtin = self.builtin_callee(func)?;
                if !matches!(builtin, Builtin::Negate | Builtin::Convert { .. }) {
                    return None;
                }
                (builtin, vec![arg.clone()])
            }
            _ => return None,
        };
        let mut vars = Vec::new();
        let mut walked = true;
        for operand in &operands {
            let d = self.solver.table.fresh();
            match self.infer_expr(scope, operand, Some(d)) {
                Some(v) => vars.push(v),
                // The operand's own walk recorded why it is not typed.
                None => walked = false,
            }
        }
        let verdict = if walked {
            self.settle(scope);
            let tys: Vec<Ty> = vars
                .iter()
                .map(|v| self.solver.table.resolve(self.solver.tys.var(*v)))
                .collect();
            let verdict = builtin_verdict(self.facts.env, self.solver.tys, &builtin, &tys);
            if let Err(gap) = verdict {
                scope.mark_incomplete(Incomplete::Builtin(gap));
            }
            Some(verdict)
        } else {
            // The operand's own walk recorded why it is not typed.
            None
        };
        let result = match (verdict, &builtin) {
            (Some(Ok(result)), _) => result,
            // A conversion's signature fixes its result (`^T -> int`), and FCS
            // unifies that with the application's type before it checks the
            // operand, so the application is `int` whatever the operand turns
            // out to be, or whenever it is solved. Only a known operand the
            // rule rejects, or may, is left untyped.
            (None | Some(Err(BuiltinGap::OpenOperand)), Builtin::Convert { target }) => self
                .solver
                .tys
                .import(self.facts.env.types(), *target, &mut HashMap::default()),
            _ => return Some(None),
        };
        let r = self.solver.table.fresh();
        self.solver.eq(self.solver.tys.var(r), result);
        Some(Some(r))
    }

    /// The [`Builtin`] the callee `e` names, if it names one: a bare name or
    /// operator token, or a module-qualified path, that the resolver resolved
    /// to the referenced assembly's member [`builtin_operator`] reads as one.
    /// A shadowing definition, in this file or an opened module, is what the
    /// resolver names instead, and is not one.
    fn builtin_callee(&self, e: &Expr) -> Option<Builtin> {
        let at = match e {
            Expr::Ident(ident) => ident.ident()?.text_range(),
            Expr::LongIdent(li) => {
                let long_ident = li.long_ident()?;
                if long_ident.active_pat_names().next().is_some() {
                    return None;
                }
                let mut idents = long_ident.idents();
                match (idents.next(), idents.next()) {
                    (Some(tok), None) => tok.text_range(),
                    _ => self.module_value_path(li)?,
                }
            }
            _ => return None,
        };
        let Some(Resolution::Member { parent, idx }) = self.facts.resolved.resolution_at(at) else {
            return None;
        };
        let Member::Method(m) = self.facts.env.member_at(parent, idx) else {
            return None;
        };
        builtin_operator(self.facts.env, parent, m)
    }
}

/// The primitive types F#'s built-in operator rules distinguish
/// (ConstraintSolver.fs: `isSignedIntegerTy`, `isUnsignedIntegerTy`,
/// `isFpTy`, `isDecimalTy`, `isCharTy`, `isStringTy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltinKind {
    /// `sbyte`, `int16`, `int`, `int64`, `nativeint`.
    Signed,
    /// `byte`, `uint16`, `uint32`, `uint64`, `unativeint`.
    Unsigned,
    /// `float32`, `float`.
    Float,
    Decimal,
    Char,
    String,
}

impl BuiltinKind {
    /// An integer, signed or not (`IsIntegerOrIntegerEnumTy`, enums aside).
    fn integer(self) -> bool {
        matches!(self, BuiltinKind::Signed | BuiltinKind::Unsigned)
    }

    /// A number (`IsNumericOrIntegralEnumType`, enums aside).
    fn numeric(self) -> bool {
        self.integer() || matches!(self, BuiltinKind::Float | BuiltinKind::Decimal)
    }
}

/// The [`BuiltinKind`] of `ty`, if it is one: exactly the type, with no
/// argument. An enum, a measure-annotated number and every other type are
/// not, and a rule over them is not modelled.
fn builtin_kind(names: &NameTable, tys: &TyTable, ty: Ty) -> Option<BuiltinKind> {
    let TyKind::Named { path, args } = tys.kind(ty) else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let path = path.segments_in(names);
    let kind = match path.as_slice() {
        ["System", "SByte" | "Int16" | "Int32" | "Int64" | "IntPtr"] => BuiltinKind::Signed,
        [
            "System",
            "Byte" | "UInt16" | "UInt32" | "UInt64" | "UIntPtr",
        ] => BuiltinKind::Unsigned,
        ["System", "Single" | "Double"] => BuiltinKind::Float,
        ["System", "Decimal"] => BuiltinKind::Decimal,
        ["System", "Char"] => BuiltinKind::Char,
        ["System", "String"] => BuiltinKind::String,
        _ => return None,
    };
    Some(kind)
}

/// Whether `ty` is a type a FSharp.Core conversion function may return: a
/// number or `char`. `ty` is a type of `tys`.
pub(crate) fn is_conversion_target(names: &NameTable, tys: &TyTable, ty: Ty) -> bool {
    builtin_kind(names, tys, ty).is_some_and(|k| k.numeric() || k == BuiltinKind::Char)
}

/// The type of an application of `builtin` to operands of types `operands`
/// (as many as it takes), by F#'s built-in rule for it, or why it is not
/// typed. Read off `SolveMemberConstraint` (ConstraintSolver.fs), over the
/// types [`builtin_kind`] names and nothing else:
///
/// - `+ - * / %`: two operands of one type, the result that type — a number,
///   or for `+` also `char` and `string`, for `-` also `char`. (`decimal`'s
///   own operator members solve it where the rule steps aside, to the same
///   type.)
/// - `&&& ||| ^^^`: two operands of one integer type, the result that type.
/// - `<<< >>>`: an integer and an `int`, the result the integer's type.
/// - `~-`: a signed integer, a float or `decimal`, the result its type.
/// - a conversion: a number, `char` or `string`, the result its target.
///
/// An operand not yet known is [`BuiltinGap::OpenOperand`]. Two different
/// operand types of which the rule takes one is [`BuiltinGap::Violated`]:
/// the rule fires and equates them (`SolveTypeEqualsTypeKeepAbbrevs`), which
/// fails. Anything else off the list is [`BuiltinGap::Undecided`] — another
/// type's own operator members (`DateTime - DateTime`), an enum, a unit of
/// measure.
///
/// `operands` and the result are types of `tys`, a session's table; a
/// conversion's target is the env's, and is copied in.
fn builtin_verdict(
    env: &AssemblyEnv,
    tys: &TyTable,
    builtin: &Builtin,
    operands: &[Ty],
) -> Result<Ty, BuiltinGap> {
    if !operands.iter().all(|&t| tys.is_ground(t)) {
        return Err(BuiltinGap::OpenOperand);
    }
    let names = env.names();
    let kinds: Vec<Option<BuiltinKind>> = operands
        .iter()
        .map(|&t| builtin_kind(names, tys, t))
        .collect();
    let is = |t: Ty, path: &[&str]| tys.is_named(names, t, path);
    let binary = |admits: &dyn Fn(BuiltinKind) -> bool| match (operands, kinds.as_slice()) {
        ([a, b], [Some(ka), Some(kb)]) if a == b && admits(*ka) => {
            debug_assert_eq!(ka, kb);
            Ok(*a)
        }
        ([a, b], [Some(ka), Some(kb)])
            if a != b
                && (admits(*ka) || admits(*kb))
                && *ka != BuiltinKind::Decimal
                && *kb != BuiltinKind::Decimal =>
        {
            Err(BuiltinGap::Violated)
        }
        _ => Err(BuiltinGap::Undecided),
    };
    match builtin {
        Builtin::Arith(Arith::Add) => {
            binary(&|k| k.numeric() || matches!(k, BuiltinKind::Char | BuiltinKind::String))
        }
        Builtin::Arith(Arith::Sub) => binary(&|k| k.numeric() || k == BuiltinKind::Char),
        Builtin::Arith(Arith::Mul | Arith::Div | Arith::Mod) => binary(&BuiltinKind::numeric),
        // `&&&`'s operands share its one type parameter, so FCS checks the
        // right operand against the left's type once it knows it, and there
        // inserts F#'s widening of an `int` (to `int64`, `nativeint` or
        // `float`): such a pair is not rejected. `+`'s three type
        // parameters are independent, so nothing is expected of its right
        // operand and nothing is widened.
        Builtin::Bitwise
            if operands
                .get(1)
                .is_some_and(|&b| is(b, &["System", "Int32"]))
                && operands.first().is_some_and(|&a| {
                    is(a, &["System", "Int64"])
                        || is(a, &["System", "IntPtr"])
                        || is(a, &["System", "Double"])
                }) =>
        {
            Err(BuiltinGap::Undecided)
        }
        Builtin::Bitwise => binary(&BuiltinKind::integer),
        Builtin::Shift => match (operands, kinds.as_slice()) {
            ([a, b], [Some(ka), _]) if ka.integer() && is(*b, &["System", "Int32"]) => Ok(*a),
            ([_, _], [Some(ka), Some(kb)]) if ka.integer() && *kb != BuiltinKind::Decimal => {
                Err(BuiltinGap::Violated)
            }
            _ => Err(BuiltinGap::Undecided),
        },
        Builtin::Negate => match (operands, kinds.as_slice()) {
            ([a], [Some(BuiltinKind::Signed | BuiltinKind::Float | BuiltinKind::Decimal)]) => {
                Ok(*a)
            }
            _ => Err(BuiltinGap::Undecided),
        },
        Builtin::Convert { target } => match kinds.as_slice() {
            [Some(k)] if k.numeric() || matches!(k, BuiltinKind::Char | BuiltinKind::String) => {
                Ok(tys.import(env.types(), *target, &mut HashMap::default()))
            }
            _ => Err(BuiltinGap::Undecided),
        },
    }
}
