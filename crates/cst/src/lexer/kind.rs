//! [`TokenKind`]: a [`Token`] without its payload.
//!
//! Every `&str` payload of a lexed [`Token`] is the token's own text (logos's
//! default for a `&str` field is the matched slice, and no callback narrows
//! it), so a token is determined by its kind and its text, and a token table
//! can store the kind alone. [`Token::from_kind`] rebuilds the token. The two
//! remaining payloads are folded into the kind or rebuilt from the text:
//!
//! * [`Token::Less`] / [`Token::Greater`] carry `false` from the lexer; the
//!   lex-filter's promotion to `true` is not a lexer fact, so the kind drops it
//!   and [`Token::from_kind`] restores `false`.
//! * [`Token::InterpString`] has one kind per [`InterpKind`] variant and byte
//!   suffix. The extended forms' `n` is the count of the text's leading `$`,
//!   which is how the lexer computes it (`callbacks::lex_interp_extended_opener`).
//!
//! See `docs/token-pipeline-soa-plan.md` (D2).

use super::{InterpKind, Token};

/// Declares [`TokenKind`] and the two conversions from one list of the
/// [`Token`] variants, so that a variant missing from the list is a
/// non-exhaustive `match` in [`Token::kind`] rather than a silent gap.
macro_rules! token_kinds {
    (
        unit: [$($unit:ident),* $(,)?],
        text: [$($text:ident),* $(,)?] $(,)?
    ) => {
        /// The kind of a lexed [`Token`]: the token without its payload, which
        /// [`Token::from_kind`] rebuilds from the token's text.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u16)]
        pub enum TokenKind {
            $($unit,)*
            $($text,)*
            Less,
            Greater,
            /// [`InterpKind::BeginEnd`] without a byte suffix.
            InterpBeginEnd,
            /// [`InterpKind::BeginEnd`] with a byte suffix.
            InterpBeginEndBytes,
            /// [`InterpKind::Begin`].
            InterpBegin,
            /// [`InterpKind::Part`].
            InterpPart,
            /// [`InterpKind::End`] without a byte suffix.
            InterpEnd,
            /// [`InterpKind::End`] with a byte suffix.
            InterpEndBytes,
            /// [`InterpKind::TripleBeginEnd`] without a byte suffix.
            InterpTripleBeginEnd,
            /// [`InterpKind::TripleBeginEnd`] with a byte suffix.
            InterpTripleBeginEndBytes,
            /// [`InterpKind::TripleBegin`].
            InterpTripleBegin,
            /// [`InterpKind::VerbatimBeginEnd`] without a byte suffix.
            InterpVerbatimBeginEnd,
            /// [`InterpKind::VerbatimBeginEnd`] with a byte suffix.
            InterpVerbatimBeginEndBytes,
            /// [`InterpKind::VerbatimBegin`].
            InterpVerbatimBegin,
            /// [`InterpKind::ExtendedBeginEnd`]; `n` is the text's leading `$`.
            InterpExtendedBeginEnd,
            /// [`InterpKind::ExtendedBegin`]; `n` is the text's leading `$`.
            InterpExtendedBegin,
        }

        impl TokenKind {
            /// Every kind, in declaration order.
            pub const ALL: &'static [TokenKind] = &[
                $(TokenKind::$unit,)*
                $(TokenKind::$text,)*
                TokenKind::Less,
                TokenKind::Greater,
                TokenKind::InterpBeginEnd,
                TokenKind::InterpBeginEndBytes,
                TokenKind::InterpBegin,
                TokenKind::InterpPart,
                TokenKind::InterpEnd,
                TokenKind::InterpEndBytes,
                TokenKind::InterpTripleBeginEnd,
                TokenKind::InterpTripleBeginEndBytes,
                TokenKind::InterpTripleBegin,
                TokenKind::InterpVerbatimBeginEnd,
                TokenKind::InterpVerbatimBeginEndBytes,
                TokenKind::InterpVerbatimBegin,
                TokenKind::InterpExtendedBeginEnd,
                TokenKind::InterpExtendedBegin,
            ];
        }

        impl<'a> Token<'a> {
            /// This token's kind: the token without its payload.
            pub fn kind(&self) -> TokenKind {
                match self {
                    $(Token::$unit => TokenKind::$unit,)*
                    $(Token::$text(_) => TokenKind::$text,)*
                    Token::Less(_) => TokenKind::Less,
                    Token::Greater(_) => TokenKind::Greater,
                    Token::InterpString(interp) => interp_token_kind(*interp),
                }
            }

            /// The token of kind `kind` whose text is `text`: the inverse of
            /// [`Token::kind`] on every token the lexer produces, given that
            /// token's text. A [`Token::Less`] / [`Token::Greater`] comes back
            /// with the lexer's `false`.
            #[inline]
            pub fn from_kind(kind: TokenKind, text: &'a str) -> Token<'a> {
                match kind {
                    $(TokenKind::$unit => Token::$unit,)*
                    $(TokenKind::$text => Token::$text(text),)*
                    TokenKind::Less => Token::Less(false),
                    TokenKind::Greater => Token::Greater(false),
                    TokenKind::InterpBeginEnd => {
                        Token::InterpString(InterpKind::BeginEnd { is_byte: false })
                    }
                    TokenKind::InterpBeginEndBytes => {
                        Token::InterpString(InterpKind::BeginEnd { is_byte: true })
                    }
                    TokenKind::InterpBegin => Token::InterpString(InterpKind::Begin),
                    TokenKind::InterpPart => Token::InterpString(InterpKind::Part),
                    TokenKind::InterpEnd => {
                        Token::InterpString(InterpKind::End { is_byte: false })
                    }
                    TokenKind::InterpEndBytes => {
                        Token::InterpString(InterpKind::End { is_byte: true })
                    }
                    TokenKind::InterpTripleBeginEnd => {
                        Token::InterpString(InterpKind::TripleBeginEnd { is_byte: false })
                    }
                    TokenKind::InterpTripleBeginEndBytes => {
                        Token::InterpString(InterpKind::TripleBeginEnd { is_byte: true })
                    }
                    TokenKind::InterpTripleBegin => Token::InterpString(InterpKind::TripleBegin),
                    TokenKind::InterpVerbatimBeginEnd => {
                        Token::InterpString(InterpKind::VerbatimBeginEnd { is_byte: false })
                    }
                    TokenKind::InterpVerbatimBeginEndBytes => {
                        Token::InterpString(InterpKind::VerbatimBeginEnd { is_byte: true })
                    }
                    TokenKind::InterpVerbatimBegin => {
                        Token::InterpString(InterpKind::VerbatimBegin)
                    }
                    TokenKind::InterpExtendedBeginEnd => Token::InterpString(
                        InterpKind::ExtendedBeginEnd { n: leading_dollars(text) },
                    ),
                    TokenKind::InterpExtendedBegin => Token::InterpString(
                        InterpKind::ExtendedBegin { n: leading_dollars(text) },
                    ),
                }
            }
        }
    };
}

token_kinds! {
    unit: [
        Whitespace, Newline, LineComment, BlockComment,
        And, As, Assert, Base, Begin, Class, Do, Done, DownTo, Else, End, Exception, False,
        Finally, For, Fun, Function, If, In, Inherit, Lazy, Let, Match, Mod, Module, Mutable,
        New, Of, Open, Or, Private, Rec, Sig, Struct, Then, To, True, Try, Type, Val, When,
        While, With, Underscore,
        Abstract, Const, Default, Delegate, Downcast, Elif, Extern, Fixed, Global, Inline,
        Interface, Internal, Member, Namespace, Null, Override, Public, Return, Static, Upcast,
        Use, Void, Yield,
        DoBang, YieldBang, ReturnBang, MatchBang, AndBang, LetBang, UseBang, WhileBang,
        TripleString, VerbatimString, String,
        LParenStarRParen, LParen, RParen, LBrack, RBrack, LBrackBar, BarRBrack, LBrackLess,
        GreaterRBrack, LQuote, LQuoteRaw, RQuote, RQuoteRaw, RQuoteDot, RQuoteRawDot,
        RQuoteBarRBrace, RQuoteRawBarRBrace, LBrace, RBrace, LBraceBar, BarRBrace, Comma,
        SemiSemi, Semi, DotDotHat, DotDot, Dot, ColonColon, ColonQMarkGreater, ColonQMark,
        ColonGreater, ColonEquals, Colon, RArrow, LArrow, Equals, AmpAmp, Amp, BarBar, Bar,
        QMarkQMark, QMark, Hash, Dollar, Tilde, Quote,
    ],
    text: [
        KeywordString, Ident, QuotedIdent, XInt, XIntSuffixed, XIEEE32, XIEEE64, IntSuffixed,
        BigNum, Decimal, Float32, Float64, IntDotDot, Int, Char, FunkyOpName, Op,
    ],
}

fn interp_token_kind(interp: InterpKind) -> TokenKind {
    match interp {
        InterpKind::BeginEnd { is_byte: false } => TokenKind::InterpBeginEnd,
        InterpKind::BeginEnd { is_byte: true } => TokenKind::InterpBeginEndBytes,
        InterpKind::Begin => TokenKind::InterpBegin,
        InterpKind::Part => TokenKind::InterpPart,
        InterpKind::End { is_byte: false } => TokenKind::InterpEnd,
        InterpKind::End { is_byte: true } => TokenKind::InterpEndBytes,
        InterpKind::TripleBeginEnd { is_byte: false } => TokenKind::InterpTripleBeginEnd,
        InterpKind::TripleBeginEnd { is_byte: true } => TokenKind::InterpTripleBeginEndBytes,
        InterpKind::TripleBegin => TokenKind::InterpTripleBegin,
        InterpKind::VerbatimBeginEnd { is_byte: false } => TokenKind::InterpVerbatimBeginEnd,
        InterpKind::VerbatimBeginEnd { is_byte: true } => TokenKind::InterpVerbatimBeginEndBytes,
        InterpKind::VerbatimBegin => TokenKind::InterpVerbatimBegin,
        InterpKind::ExtendedBeginEnd { .. } => TokenKind::InterpExtendedBeginEnd,
        InterpKind::ExtendedBegin { .. } => TokenKind::InterpExtendedBegin,
    }
}

/// The count of `text`'s leading `$`: an extended interpolated string's
/// delimiter length.
fn leading_dollars(text: &str) -> usize {
    text.bytes().take_while(|&b| b == b'$').count()
}
