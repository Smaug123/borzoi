//! F#'s operator-name mangling: the name FCS's name environment holds an
//! operator under.
//!
//! FCS's parser never keeps an operator's spelling as the identifier: every
//! operator ident — an infix use `a + b`, a parenthesised value `(+)`, a
//! definition `let (+) a b = …` — carries `CompileOpName "+"` = `op_Addition`
//! in `Ident.idText`, with the spelling kept only as trivia. So `+` and
//! `op_Addition` are one name: `let (+) a b = a` is what `op_Addition 1 2`
//! calls, and `let op_Addition a b = a` is what `1 + 2` calls. A resolver that
//! keys a value by its spelling sees two names where F# sees one.
//!
//! A *prefix* application is the exception, and is not handled here: FCS names
//! the operator in `-x` `~-` (`op_UnaryNegation`), not `-`, so the name depends
//! on the position, not on the token.
//!
//! Both directions are ports from `src/Compiler/SyntaxTree/PrettyNaming.fs`:
//! [`compile_op_name`] of `CompileOpName`, restricted to operator spellings
//! (FCS applies it only to operator tokens; on an identifier it is the
//! identity, and on anything else, a keyword, it is never called, so this
//! leaves every non-operator spelling alone rather than mangling it); and
//! [`operator_display_name`] of `ConvertValLogicalNameToDisplayName`'s
//! operator arm, the `(+)` a value's logical name `op_Addition` is shown as.

use std::borrow::Cow;

/// `PrettyNaming.opNameTable`: the operators with a dedicated compiled name.
const OP_NAME_TABLE: &[(&str, &str)] = &[
    ("[]", "op_Nil"),
    ("::", "op_ColonColon"),
    ("+", "op_Addition"),
    ("~%", "op_Splice"),
    ("~%%", "op_SpliceUntyped"),
    ("~++", "op_Increment"),
    ("~--", "op_Decrement"),
    ("-", "op_Subtraction"),
    ("*", "op_Multiply"),
    ("**", "op_Exponentiation"),
    ("/", "op_Division"),
    ("@", "op_Append"),
    ("^", "op_Concatenate"),
    ("%", "op_Modulus"),
    ("&&&", "op_BitwiseAnd"),
    ("|||", "op_BitwiseOr"),
    ("^^^", "op_ExclusiveOr"),
    ("<<<", "op_LeftShift"),
    ("~~~", "op_LogicalNot"),
    (">>>", "op_RightShift"),
    ("~+", "op_UnaryPlus"),
    ("~-", "op_UnaryNegation"),
    ("~&", "op_AddressOf"),
    ("~&&", "op_IntegerAddressOf"),
    ("&&", "op_BooleanAnd"),
    ("||", "op_BooleanOr"),
    ("<=", "op_LessThanOrEqual"),
    ("=", "op_Equality"),
    ("<>", "op_Inequality"),
    (">=", "op_GreaterThanOrEqual"),
    ("<", "op_LessThan"),
    (">", "op_GreaterThan"),
    ("|>", "op_PipeRight"),
    ("||>", "op_PipeRight2"),
    ("|||>", "op_PipeRight3"),
    ("<|", "op_PipeLeft"),
    ("<||", "op_PipeLeft2"),
    ("<|||", "op_PipeLeft3"),
    ("!", "op_Dereference"),
    (">>", "op_ComposeRight"),
    ("<<", "op_ComposeLeft"),
    ("<< >>", "op_TypedQuotationUnicode"),
    ("<<| |>>", "op_ChevronsBar"),
    ("<@ @>", "op_Quotation"),
    ("<@@ @@>", "op_QuotationUntyped"),
    ("+=", "op_AdditionAssignment"),
    ("-=", "op_SubtractionAssignment"),
    ("*=", "op_MultiplyAssignment"),
    ("/=", "op_DivisionAssignment"),
    ("..", "op_Range"),
    (".. ..", "op_RangeStep"),
    ("?", "op_Dynamic"),
    ("?<-", "op_DynamicAssignment"),
    (".()", "op_ArrayLookup"),
    (".()<-", "op_ArrayAssign"),
];

/// `PrettyNaming.opCharTranslateTable`: how `compileCustomOpName` spells each
/// operator character.
const OP_CHAR_TABLE: &[(char, &str)] = &[
    ('>', "Greater"),
    ('<', "Less"),
    ('+', "Plus"),
    ('-', "Minus"),
    ('*', "Multiply"),
    ('=', "Equals"),
    ('~', "Twiddle"),
    ('%', "Percent"),
    ('.', "Dot"),
    ('$', "Dollar"),
    ('&', "Amp"),
    ('|', "Bar"),
    ('@', "At"),
    ('#', "Hash"),
    ('^', "Hat"),
    ('!', "Bang"),
    ('?', "Qmark"),
    ('/', "Divide"),
    (':', "Colon"),
    ('(', "LParen"),
    (',', "Comma"),
    (')', "RParen"),
    (' ', "Space"),
    ('[', "LBrack"),
    (']', "RBrack"),
];

fn op_char_name(c: char) -> Option<&'static str> {
    OP_CHAR_TABLE
        .iter()
        .find_map(|&(k, v)| (k == c).then_some(v))
}

/// Whether `spelling` is an operator: a table entry, or a non-empty run of
/// operator characters (`PrettyNaming.IsOperatorDisplayName` on the core name,
/// which excludes the space). An identifier, a keyword, and an active-pattern
/// name (`|A|_|`, which has letters) are not.
pub fn is_operator_spelling(spelling: &str) -> bool {
    OP_NAME_TABLE.iter().any(|&(k, _)| k == spelling)
        || (!spelling.is_empty()
            && spelling
                .chars()
                .all(|c| c != ' ' && op_char_name(c).is_some()))
}

/// The name FCS's parser gives the operator spelled `spelling`
/// (`PrettyNaming.CompileOpName`), or `spelling` itself when it is not an
/// operator.
///
/// `+` is `op_Addition`; a custom operator spells each character, so `+.` is
/// `op_PlusDot` and `>>=` is `op_GreaterGreaterEquals`.
pub fn compile_op_name(spelling: &str) -> Cow<'_, str> {
    if let Some(&(_, compiled)) = OP_NAME_TABLE.iter().find(|&&(k, _)| k == spelling) {
        return Cow::Borrowed(compiled);
    }
    if !is_operator_spelling(spelling) {
        return Cow::Borrowed(spelling);
    }
    let mut out = String::from("op_");
    for c in spelling.chars() {
        out.push_str(op_char_name(c).expect("an operator spelling has only operator characters"));
    }
    Cow::Owned(out)
}

/// The operator spelling a compiled name stands for
/// (`ConvertValLogicalNameToDisplayNameCore`): a table name's operator, or a
/// custom `op_*` name spelled back character by character. `None` when the
/// name is not `op_*`, or does not decompile (`op_Implicit`).
///
/// Each step takes the first character name, in table order, that the rest
/// of the name starts with, exactly as FCS's `decompileCustomOpName` does.
fn decompile_op_name(logical: &str) -> Option<Cow<'static, str>> {
    if let Some(&(op, _)) = OP_NAME_TABLE.iter().find(|&&(_, c)| c == logical) {
        return Some(Cow::Borrowed(op));
    }
    let mut rest = logical.strip_prefix("op_")?;
    let mut out = String::new();
    while !rest.is_empty() {
        let &(c, name) = OP_CHAR_TABLE
            .iter()
            .find(|&&(_, name)| rest.starts_with(name))?;
        out.push(c);
        rest = &rest[name.len()..];
    }
    (!out.is_empty()).then_some(Cow::Owned(out))
}

/// How FCS shows a value or member whose logical name is an operator's
/// compiled name: `op_Addition` is `(+)`, and an operator that starts or ends
/// with `*` (other than `*` itself) is spaced, `( ** )`, so it cannot open or
/// close a comment. `None` for every other name, which renders as an ordinary
/// identifier.
pub fn operator_display_name(logical: &str) -> Option<String> {
    let op = decompile_op_name(logical)?;
    Some(if op != "*" && (op.starts_with('*') || op.ends_with('*')) {
        format!("( {op} )")
    } else {
        format!("({op})")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_operators_take_their_dedicated_name() {
        assert_eq!(compile_op_name("+"), "op_Addition");
        assert_eq!(compile_op_name("|>"), "op_PipeRight");
        assert_eq!(compile_op_name("~-"), "op_UnaryNegation");
        assert_eq!(compile_op_name("::"), "op_ColonColon");
    }

    #[test]
    fn custom_operators_spell_each_character() {
        assert_eq!(compile_op_name("+."), "op_PlusDot");
        assert_eq!(compile_op_name(">>="), "op_GreaterGreaterEquals");
        assert_eq!(compile_op_name("<*>"), "op_LessMultiplyGreater");
        assert_eq!(compile_op_name(":="), "op_ColonEquals");
    }

    #[test]
    fn compiled_names_display_as_fcs_shows_them() {
        assert_eq!(operator_display_name("op_Addition").as_deref(), Some("(+)"));
        assert_eq!(operator_display_name("op_Multiply").as_deref(), Some("(*)"));
        assert_eq!(
            operator_display_name("op_Exponentiation").as_deref(),
            Some("( ** )")
        );
        assert_eq!(operator_display_name("op_PlusDot").as_deref(), Some("(+.)"));
        assert_eq!(operator_display_name("op_Implicit"), None);
        assert_eq!(operator_display_name("Length"), None);
    }

    proptest::proptest! {
        /// Compiling then displaying an operator spelling gives back the
        /// spelling: the two tables are one bijection on what F# can define.
        #[test]
        fn display_inverts_compile(op in "[!$%&*+./<=>?@^|~:-]{1,4}") {
            let compiled = compile_op_name(&op);
            let shown = operator_display_name(&compiled).expect("an operator decompiles");
            let inner = shown.trim_start_matches('(').trim_end_matches(')').trim();
            proptest::prop_assert_eq!(inner, op.as_str());
        }
    }

    #[test]
    fn non_operators_are_left_alone() {
        for s in ["x", "op_Addition", "|A|_|", "|A|", "base", "_", "", "a+"] {
            assert_eq!(compile_op_name(s), s, "{s:?}");
        }
    }
}
