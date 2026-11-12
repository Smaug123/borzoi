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
//! A *prefix* application is the exception: FCS names the operator in `-x`
//! `~-` (`op_UnaryNegation`), not `-`, so the name depends on the position, not
//! only on the token. [`prefix_op_name`] is that position's name.
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

/// The bytes of `OP_CHAR_TABLE`'s characters, as a set: bit `b` is set for
/// each character whose code is `b`. Every one is ASCII (asserted here), so no
/// byte of a multi-byte UTF-8 character is in the set, and a spelling's bytes
/// are all in it exactly when its characters are all in the table.
const OP_CHAR_BYTES: u128 = {
    let mut set = 0u128;
    let mut i = 0;
    while i < OP_CHAR_TABLE.len() {
        let c = OP_CHAR_TABLE[i].0;
        assert!(c.is_ascii(), "operator characters are ASCII");
        set |= 1 << c as u32;
        i += 1;
    }
    set
};

const fn is_op_char_byte(b: u8) -> bool {
    b < 128 && OP_CHAR_BYTES >> b & 1 == 1
}

// Every `OP_NAME_TABLE` entry is a non-empty run of operator characters (the
// space among them), so a spelling with any other byte is no entry, and
// `classify` rejects it without a lookup.
const _: () = {
    let mut i = 0;
    while i < OP_NAME_TABLE.len() {
        let entry = OP_NAME_TABLE[i].0.as_bytes();
        assert!(!entry.is_empty(), "an operator table entry is non-empty");
        let mut j = 0;
        while j < entry.len() {
            assert!(
                is_op_char_byte(entry[j]),
                "an operator table entry has only operator characters"
            );
            j += 1;
        }
        i += 1;
    }
};

/// What a spelling is to `PrettyNaming.CompileOpName`.
enum OpSpelling {
    /// Not an operator: an identifier, a keyword, an active-pattern name.
    NotOperator,
    /// An `OP_NAME_TABLE` entry, with its dedicated compiled name.
    Table(&'static str),
    /// A non-empty run of operator characters, the space excluded, that is no
    /// table entry: compiled character by character.
    Custom,
}

fn classify(spelling: &str) -> OpSpelling {
    // Nearly every spelling is an identifier, which fails this on its first
    // byte; every operator, from the table or custom, passes it.
    if spelling.is_empty() || !spelling.bytes().all(is_op_char_byte) {
        return OpSpelling::NotOperator;
    }
    if let Some(&(_, compiled)) = OP_NAME_TABLE.iter().find(|&&(k, _)| k == spelling) {
        OpSpelling::Table(compiled)
    } else if spelling.contains(' ') {
        OpSpelling::NotOperator
    } else {
        OpSpelling::Custom
    }
}

/// Whether `spelling` is an operator: a table entry, or a non-empty run of
/// operator characters (`PrettyNaming.IsOperatorDisplayName` on the core name,
/// which excludes the space). An identifier, a keyword, and an active-pattern
/// name (`|A|_|`, which has letters) are not.
pub fn is_operator_spelling(spelling: &str) -> bool {
    !matches!(classify(spelling), OpSpelling::NotOperator)
}

/// The name FCS's parser gives the operator spelled `spelling`
/// (`PrettyNaming.CompileOpName`), or `spelling` itself when it is not an
/// operator.
///
/// `+` is `op_Addition`; a custom operator spells each character, so `+.` is
/// `op_PlusDot` and `>>=` is `op_GreaterGreaterEquals`.
pub fn compile_op_name(spelling: &str) -> Cow<'_, str> {
    match classify(spelling) {
        OpSpelling::NotOperator => Cow::Borrowed(spelling),
        OpSpelling::Table(compiled) => Cow::Borrowed(compiled),
        OpSpelling::Custom => {
            let mut out = String::from("op_");
            for c in spelling.chars() {
                out.push_str(
                    op_char_name(c).expect("a custom operator has only operator characters"),
                );
            }
            Cow::Owned(out)
        }
    }
}

/// The name FCS's parser gives the operator of a *prefix* application whose
/// token is spelled `spelling`.
///
/// The grammar decides it by token class (`pars.fsy`'s `minusExpr`, `argExpr`
/// and `atomicExpr` rules). A `!` or `~` operator (`PREFIX_OP`, through
/// `mkSynPrefixPrim`) keeps its spelling, so `!r` uses `!` and `~~~n` uses
/// `~~~` — the same names their definitions `( ! )` and `( ~~~ )` have. Every
/// other prefix operator (`-`, `+`, `-.`, `+.`, `%`, `%%`, `?-`, through
/// `mkSynPrefix`) is renamed `~` + its spelling, so `-x` uses `~-`
/// (`op_UnaryNegation`), not the binary `-`. The token class is read past the
/// lexer's `ignored_op_char`s (`.`, `$`, `?`), as `lex.fsl` reads it.
/// `&`/`&&` prefixes are address-of expressions, never operator uses.
pub fn prefix_op_name(spelling: &str) -> Cow<'_, str> {
    let class = spelling.chars().find(|c| !matches!(c, '.' | '$' | '?'));
    if matches!(class, Some('!' | '~')) || !is_operator_spelling(spelling) {
        compile_op_name(spelling)
    } else {
        Cow::Owned(compile_op_name(&format!("~{spelling}")).into_owned())
    }
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
    fn a_prefix_use_is_named_by_its_token_class() {
        assert_eq!(prefix_op_name("-"), "op_UnaryNegation");
        assert_eq!(prefix_op_name("+"), "op_UnaryPlus");
        assert_eq!(prefix_op_name("-."), "op_TwiddleMinusDot");
        assert_eq!(prefix_op_name("%"), "op_Splice");
        assert_eq!(prefix_op_name("%%"), "op_SpliceUntyped");
        assert_eq!(prefix_op_name("?-"), "op_TwiddleQmarkMinus");
        assert_eq!(prefix_op_name("!"), "op_Dereference");
        assert_eq!(prefix_op_name("~~~"), "op_LogicalNot");
        assert_eq!(prefix_op_name("~~"), "op_TwiddleTwiddle");
        assert_eq!(prefix_op_name("!!"), "op_BangBang");
        assert_eq!(prefix_op_name("x"), "x");
    }

    /// `PrettyNaming.IsValidPrefixOperatorDefinitionName`: what `let ( op )`
    /// may name for a prefix use to reach it.
    fn is_valid_prefix_definition_name(s: &str) -> bool {
        match s.chars().next() {
            Some('~') => {
                s.chars().all(|c| c == '~')
                    || matches!(
                        s,
                        "~?+" | "~?-" | "~+" | "~-" | "~+." | "~-." | "~%" | "~%%" | "~&" | "~&&"
                    )
            }
            Some('!') => s != "!=",
            _ => false,
        }
    }

    /// The spellings FCS accepts as a prefix operator use
    /// (`PrettyNaming.IsValidPrefixOperatorUse`) that lex as an operator
    /// token, less `&`/`&&`, which are address-of.
    fn prefix_use() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        prop_oneof![
            proptest::sample::select(vec!["?+", "?-", "+", "-", "+.", "-.", "%", "%%"])
                .prop_map(String::from),
            "~{1,4}",
            // `!=` lexes as a comparison, never a prefix.
            "![!$%&*+./<>?@^|~:-][!$%&*+./<=>?@^|~:-]{0,2}",
            Just("!".to_string()),
        ]
    }

    proptest::proptest! {
        /// A prefix use always names something a prefix definition can
        /// define: `-x` reaches `let ( ~- ) x`, `!r` reaches `let ( ! ) r`.
        #[test]
        fn a_prefix_use_names_a_prefix_definition(op in prefix_use()) {
            let name = prefix_op_name(&op);
            let shown = operator_display_name(&name).expect("an operator decompiles");
            let spelled = shown.trim_start_matches('(').trim_end_matches(')').trim();
            proptest::prop_assert!(is_valid_prefix_definition_name(spelled), "{op} -> {spelled}");
        }

        /// Only a `!` or `~` spelling shares its name with the binary
        /// operator of that spelling.
        #[test]
        fn only_bang_and_tilde_keep_their_spelling(op in prefix_use()) {
            let keeps = op.starts_with('!') || op.starts_with('~');
            proptest::prop_assert_eq!(prefix_op_name(&op) == compile_op_name(&op), keeps);
        }
    }

    #[test]
    fn non_operators_are_left_alone() {
        for s in ["x", "op_Addition", "|A|_|", "|A|", "base", "_", "", "a+"] {
            assert_eq!(compile_op_name(s), s, "{s:?}");
        }
    }

    /// The reference for [`is_operator_spelling`]: `IsOperatorDisplayName`
    /// read off the two tables by linear scan, character by character.
    fn reference_is_operator_spelling(spelling: &str) -> bool {
        OP_NAME_TABLE.iter().any(|&(k, _)| k == spelling)
            || (!spelling.is_empty()
                && spelling
                    .chars()
                    .all(|c| c != ' ' && OP_CHAR_TABLE.iter().any(|&(k, _)| k == c)))
    }

    /// The reference for [`compile_op_name`], by linear scan of the tables.
    fn reference_compile_op_name(spelling: &str) -> Cow<'_, str> {
        if let Some(&(_, compiled)) = OP_NAME_TABLE.iter().find(|&&(k, _)| k == spelling) {
            return Cow::Borrowed(compiled);
        }
        if !reference_is_operator_spelling(spelling) {
            return Cow::Borrowed(spelling);
        }
        let mut out = String::from("op_");
        for c in spelling.chars() {
            let &(_, name) = OP_CHAR_TABLE
                .iter()
                .find(|&&(k, _)| k == c)
                .expect("an operator spelling has only operator characters");
            out.push_str(name);
        }
        Cow::Owned(out)
    }

    /// Spellings built from pieces weighted towards what the fast paths
    /// branch on: whole table entries (so spliced into other text when
    /// several are drawn), operator characters, the space that only table
    /// entries may hold, identifier characters, backticks, and non-ASCII
    /// characters (whose UTF-8 bytes must not read as operator characters).
    fn spelling() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        let table_entry =
            proptest::sample::select(OP_NAME_TABLE.iter().map(|&(k, _)| k).collect::<Vec<_>>())
                .prop_map(String::from);
        // The entries with a space are few, and only such an entry, exactly,
        // makes a spelling with a space an operator.
        let spaced_entry = proptest::sample::select(
            OP_NAME_TABLE
                .iter()
                .map(|&(k, _)| k)
                .filter(|k| k.contains(' '))
                .collect::<Vec<_>>(),
        )
        .prop_map(String::from);
        let op_char =
            proptest::sample::select(OP_CHAR_TABLE.iter().map(|&(c, _)| c).collect::<Vec<_>>())
                .prop_map(String::from);
        let piece = prop_oneof![
            4 => table_entry,
            2 => spaced_entry,
            4 => op_char,
            1 => Just(" ".to_string()),
            2 => "[A-Za-z0-9_']",
            1 => Just("``".to_string()),
            1 => Just("`".to_string()),
            1 => proptest::sample::select(vec!["é", "→", "\u{80}", "\u{3c}\u{301}", "\0"])
                .prop_map(String::from),
        ];
        proptest::collection::vec(piece, 0..5).prop_map(|pieces| pieces.concat())
    }

    /// What [`spelling`] must reach for the equivalence property to say
    /// anything about each branch of the fast path.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum SpellingClass {
        Empty,
        /// A table entry exactly, one without a space.
        TableEntry,
        /// A table entry exactly, one with a space (a non-operator otherwise).
        SpacedTableEntry,
        /// A run of operator characters that is not a table entry.
        CustomOperator,
        /// Operator characters and a space, not a table entry.
        SpacedNonEntry,
        /// A table entry inside a longer non-operator spelling.
        EntryInText,
        /// Anything else that is not an operator.
        Other,
    }

    fn classify_for_coverage(s: &str) -> SpellingClass {
        let entry = OP_NAME_TABLE.iter().any(|&(k, _)| k == s);
        let op_chars = OP_CHAR_TABLE
            .iter()
            .map(|&(c, _)| c)
            .filter(|&c| c != ' ')
            .collect::<Vec<_>>();
        let all_op = !s.is_empty() && s.chars().all(|c| op_chars.contains(&c));
        let all_op_or_space = !s.is_empty() && s.chars().all(|c| c == ' ' || op_chars.contains(&c));
        if s.is_empty() {
            SpellingClass::Empty
        } else if entry && s.contains(' ') {
            SpellingClass::SpacedTableEntry
        } else if entry {
            SpellingClass::TableEntry
        } else if all_op {
            SpellingClass::CustomOperator
        } else if all_op_or_space {
            SpellingClass::SpacedNonEntry
        } else if OP_NAME_TABLE.iter().any(|&(k, _)| s.contains(k)) {
            SpellingClass::EntryInText
        } else {
            SpellingClass::Other
        }
    }

    const SPELLING_CASES: u32 = 4096;

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(SPELLING_CASES))]

        /// The fast paths answer exactly as the linear scans of the tables do.
        #[test]
        fn fast_paths_agree_with_the_table_scan(s in spelling()) {
            proptest::prop_assert_eq!(
                is_operator_spelling(&s),
                reference_is_operator_spelling(&s),
                "is_operator_spelling({:?})", s
            );
            proptest::prop_assert_eq!(
                compile_op_name(&s),
                reference_compile_op_name(&s),
                "compile_op_name({:?})", s
            );
        }
    }

    /// Every table entry and every operator character, alone, agree too: the
    /// generator draws them, but this makes the claim for each one.
    #[test]
    fn every_table_entry_and_operator_character_agrees() {
        let singles = OP_NAME_TABLE
            .iter()
            .map(|&(k, _)| k.to_string())
            .chain(OP_CHAR_TABLE.iter().map(|&(c, _)| c.to_string()));
        for s in singles {
            assert_eq!(
                is_operator_spelling(&s),
                reference_is_operator_spelling(&s),
                "{s:?}"
            );
            assert_eq!(compile_op_name(&s), reference_compile_op_name(&s), "{s:?}");
        }
    }

    /// [`spelling`] reaches every class the fast path distinguishes, each
    /// often enough that the property's case budget samples it many times.
    #[test]
    fn the_spelling_generator_reaches_every_class() {
        use proptest::strategy::{Strategy, ValueTree};
        let mut runner = proptest::test_runner::TestRunner::deterministic();
        let strategy = spelling();
        let mut counts = std::collections::HashMap::<SpellingClass, u32>::new();
        for _ in 0..SPELLING_CASES {
            let s = strategy
                .new_tree(&mut runner)
                .expect("generate a spelling")
                .current();
            *counts.entry(classify_for_coverage(&s)).or_default() += 1;
        }
        for class in [
            SpellingClass::Empty,
            SpellingClass::TableEntry,
            SpellingClass::SpacedTableEntry,
            SpellingClass::CustomOperator,
            SpellingClass::SpacedNonEntry,
            SpellingClass::EntryInText,
            SpellingClass::Other,
        ] {
            let n = counts.get(&class).copied().unwrap_or(0);
            assert!(
                n >= 20,
                "{class:?} drawn {n} times in {SPELLING_CASES}: {counts:?}"
            );
        }
    }
}
