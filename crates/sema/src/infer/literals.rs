//! The types of literals, and the values reachability compares them by.

use super::*;

pub(super) fn literal_ty(names: &NameTable, tys: &TyTable, c: &ConstExpr) -> Option<Ty> {
    literal_kind_ty(names, tys, c.literal()?.kind())
}

/// The value a constant pattern's literal spells, as reachability compares
/// them ([`Ctor::Const`]): its text when that is the only spelling of its
/// value — a decimal `int` without leading zeros or separators, a string or
/// character with no escape — so two such spellings differ exactly when the
/// values do; else no value, which may equal any other (`1` and `0x1`).
pub(super) fn literal_ctor(kind: SyntaxKind, text: &str) -> Ctor<UnionId> {
    let canonical = |ok: bool| ok.then(|| Box::<str>::from(text));
    match kind {
        SyntaxKind::BOOL_LIT => Ctor::Bool(text == "true"),
        SyntaxKind::INT32_LIT => {
            let digits = text.strip_prefix('-').unwrap_or(text);
            let ok = !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'))
                && text != "-0";
            Ctor::Const {
                kind: ConstKind::Int,
                value: canonical(ok),
            }
        }
        SyntaxKind::STRING_LIT => Ctor::Const {
            kind: ConstKind::Str,
            value: canonical(
                text.len() >= 2
                    && text.starts_with('"')
                    && text.ends_with('"')
                    && !text.contains('\\'),
            ),
        },
        SyntaxKind::CHAR_LIT => Ctor::Const {
            kind: ConstKind::Char,
            value: canonical(
                text.len() >= 3
                    && text.starts_with('\'')
                    && text.ends_with('\'')
                    && !text.contains('\\'),
            ),
        },
        _ => unreachable!("a constant pattern of a literal inference does not read: {kind:?}"),
    }
}

/// The type of a literal token of `kind`, where it is fixed in isolation
/// ([`literal_ty`]).
pub(super) fn literal_kind_ty(names: &NameTable, tys: &TyTable, kind: SyntaxKind) -> Option<Ty> {
    let prim = |path: &str| Some(tys.named(names, path));
    match kind {
        // Integers (default and every explicit suffix).
        SyntaxKind::INT32_LIT => prim("System.Int32"),
        SyntaxKind::SBYTE_LIT => prim("System.SByte"),
        SyntaxKind::BYTE_LIT => prim("System.Byte"),
        SyntaxKind::INT16_LIT => prim("System.Int16"),
        SyntaxKind::UINT16_LIT => prim("System.UInt16"),
        SyntaxKind::UINT32_LIT => prim("System.UInt32"),
        SyntaxKind::INT64_LIT => prim("System.Int64"),
        SyntaxKind::UINT64_LIT => prim("System.UInt64"),
        SyntaxKind::INTPTR_LIT => prim("System.IntPtr"),
        SyntaxKind::UINTPTR_LIT => prim("System.UIntPtr"),
        // Floating point and decimal.
        SyntaxKind::IEEE64_LIT => prim("System.Double"),
        SyntaxKind::IEEE32_LIT => prim("System.Single"),
        SyntaxKind::DECIMAL_LIT => prim("System.Decimal"),
        // Text and characters.
        SyntaxKind::STRING_LIT
        | SyntaxKind::VERBATIM_STRING_LIT
        | SyntaxKind::TRIPLE_STRING_LIT => prim("System.String"),
        SyntaxKind::CHAR_LIT => prim("System.Char"),
        SyntaxKind::BOOL_LIT => prim("System.Boolean"),
        // `()`: the multi-token `SynConst.Unit`, whose first token is the `(`.
        SyntaxKind::LPAREN_TOK => prim(UNIT),
        // Byte strings are `byte[]` (the `op_Implicit` to `ReadOnlySpan<byte>`
        // fires at a *use* site, not here — the bound value stays `byte[]`).
        SyntaxKind::BYTE_STRING_LIT
        | SyntaxKind::VERBATIM_BYTE_STRING_LIT
        | SyntaxKind::TRIPLE_BYTE_STRING_LIT => Some(tys.array(tys.named(names, "System.Byte"), 1)),
        // Deferred even in a no-expected-type position: a user-defined numeric
        // literal (`USER_NUM_LIT`, e.g. `1I`/`1G`) resolves through whichever
        // `NumericLiteral` module is in scope, and a source-location identifier
        // (`__LINE__` is int, `__SOURCE_FILE__` is string) has no single type.
        _ => None,
    }
}
