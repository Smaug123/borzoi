//! FSharp.Core's infix operators that inference types, and the facts both the
//! resolver and inference need about each.
//!
//! An operator's *spelling* (`+`) is not how FCS finds it: the name
//! environment holds the **compiled** name (`op_Addition`), under which a
//! definition spelled either way (`let (+) …`, `let op_Addition …`) shadows
//! FSharp.Core's. Inference types an application of the operator by
//! [`CoreOperator::typing`] only when no such definition exists anywhere it
//! could come from — the file, an earlier file, a referenced assembly other
//! than FSharp.Core — which is checked by both names.

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
    pub(crate) typing: OperatorTyping,
}

const fn op(
    spelling: &'static str,
    compiled: &'static str,
    typing: OperatorTyping,
) -> CoreOperator {
    CoreOperator {
        spelling,
        compiled,
        typing,
    }
}

/// The operators inference types: FSharp.Core's `Operators` arithmetic and
/// comparisons. `&&` and `||` are elaborated by FCS as control flow, not calls,
/// and are left for a slice of their own.
pub(crate) const CORE_OPERATORS: &[CoreOperator] = &[
    op("+", "op_Addition", OperatorTyping::Add),
    op("-", "op_Subtraction", OperatorTyping::Arithmetic),
    op("*", "op_Multiply", OperatorTyping::Arithmetic),
    op("/", "op_Division", OperatorTyping::Arithmetic),
    op("%", "op_Modulus", OperatorTyping::Arithmetic),
    op("=", "op_Equality", OperatorTyping::Comparison),
    op("<>", "op_Inequality", OperatorTyping::Comparison),
    op("<", "op_LessThan", OperatorTyping::Comparison),
    op(">", "op_GreaterThan", OperatorTyping::Comparison),
    op("<=", "op_LessThanOrEqual", OperatorTyping::Comparison),
    op(">=", "op_GreaterThanOrEqual", OperatorTyping::Comparison),
];

/// The table entry for an operator spelling, if inference types it.
pub(crate) fn core_operator(spelling: &str) -> Option<&'static CoreOperator> {
    CORE_OPERATORS.iter().find(|o| o.spelling == spelling)
}
