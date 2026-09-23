//! FSharp.Core's infix operators that inference types, and the facts both the
//! resolver and inference need about each.
//!
//! An operator's *spelling* (`+`) is not how FCS finds it: the name
//! environment holds the **compiled** name (`op_Addition`), under which a user
//! definition spelled either way (`let (+) …`, `let op_Addition …`) shadows
//! FSharp.Core's. So the resolver records, per operator token, what the
//! spelling resolves to — or, when nothing answers to the spelling, what the
//! compiled name resolves to ([`crate::ResolvedFile::operator_target_at`]).
//! Inference types an application of the operator only when that is
//! FSharp.Core's own member ([`CoreOperator::module`] and
//! [`CoreOperator::compiled`]), and then by [`CoreOperator::typing`].

/// How FSharp.Core types an operator over two ground operands of one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperatorTyping {
    /// `+`: a numeric primitive, `char` or `string`, typed as the operands.
    Add,
    /// `-`, `*`, `/`, `%`: a numeric primitive, typed as the operands.
    Arithmetic,
    /// `=`, `<>`, `<`, `>`, `<=`, `>=` over an equality/comparison type:
    /// `bool`.
    Comparison,
}

/// One FSharp.Core infix operator.
#[derive(Debug)]
pub(crate) struct CoreOperator {
    /// The source spelling.
    pub(crate) spelling: &'static str,
    /// The compiled (and name-environment) name.
    pub(crate) compiled: &'static str,
    /// The F# source full name of the FSharp.Core module that declares it.
    pub(crate) module: &'static str,
    pub(crate) typing: OperatorTyping,
}

const OPERATORS: &str = "Microsoft.FSharp.Core.Operators";

const fn op(
    spelling: &'static str,
    compiled: &'static str,
    module: &'static str,
    typing: OperatorTyping,
) -> CoreOperator {
    CoreOperator {
        spelling,
        compiled,
        module,
        typing,
    }
}

/// The operators inference types. `&&` and `||` are not here: FSharp.Core
/// declares them in `LanguagePrimitives.IntrinsicOperators`, a module-shaped
/// auto-open the resolver does not fold (issue #50), so no lookup could prove
/// a use of either is FSharp.Core's.
pub(crate) const CORE_OPERATORS: &[CoreOperator] = &[
    op("+", "op_Addition", OPERATORS, OperatorTyping::Add),
    op("-", "op_Subtraction", OPERATORS, OperatorTyping::Arithmetic),
    op("*", "op_Multiply", OPERATORS, OperatorTyping::Arithmetic),
    op("/", "op_Division", OPERATORS, OperatorTyping::Arithmetic),
    op("%", "op_Modulus", OPERATORS, OperatorTyping::Arithmetic),
    op("=", "op_Equality", OPERATORS, OperatorTyping::Comparison),
    op("<>", "op_Inequality", OPERATORS, OperatorTyping::Comparison),
    op("<", "op_LessThan", OPERATORS, OperatorTyping::Comparison),
    op(">", "op_GreaterThan", OPERATORS, OperatorTyping::Comparison),
    op(
        "<=",
        "op_LessThanOrEqual",
        OPERATORS,
        OperatorTyping::Comparison,
    ),
    op(
        ">=",
        "op_GreaterThanOrEqual",
        OPERATORS,
        OperatorTyping::Comparison,
    ),
];

/// The table entry for an operator spelling, if inference types it.
pub(crate) fn core_operator(spelling: &str) -> Option<&'static CoreOperator> {
    CORE_OPERATORS.iter().find(|o| o.spelling == spelling)
}
