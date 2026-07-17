//! Differential test: our Rust lexer vs FCS's pre-LexFilter token stream.
//!
//! Drives `tools/fcs-dump tokens-raw` against a small set of source snippets
//! and asserts our `Token` stream agrees with FCS on token kind and byte
//! range. This is the oracle the LexFilter port is checked against: any
//! disagreement here means the foundation is wrong.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
//!
//! Shared helpers (Rust→FCS kind mapping, JSON parsing, divergence reporter)
//! live in `tests/all/common/mod.rs` — they're reused by `tests/all/lexfilter_diff/`.

use std::collections::HashSet;
use std::io::Write;

use borzoi_cst::directives::lex_with_symbols;
use tempfile::NamedTempFile;

use crate::common::{
    NormalisedToken, fcs_tokens_raw_batch, is_trivia, parse_fcs_dump, project_dir,
    report_divergence, rust_kind_name,
};

#[test]
fn diff_trivial_let_binding() {
    assert_streams_match("let x = 1 + 2\n");
}

#[test]
fn diff_string_literal() {
    assert_streams_match("let s = \"hello\"\n");
}

#[test]
fn diff_integer_suffixes() {
    assert_streams_match("let a = 42L\nlet b = 0xFFuy\nlet c = 0b1010u\n");
}

#[test]
fn diff_operators_assorted() {
    // Exercises every operator-precedence bucket lex.fsl distinguishes:
    // **, *, /, %, +, -, @, ^, =, <, >, &, |, ! prefix.
    assert_streams_match("let r = 1 ** 2 * 3 / 4 % 5 + 6 - 7 @ 8 ^ 9\n");
}

#[test]
fn diff_punctuation() {
    assert_streams_match("let xs = [|1; 2|]\nlet ys = {| a = 1 |}\n");
}

#[test]
fn diff_float_literals() {
    assert_streams_match("let a = 1.5\nlet b = 1.0e10\nlet c = 1.5f\nlet d = 1.5m\nlet e = 1I\n");
}

#[test]
fn diff_hex_literals() {
    // `0x1lf` is the actual `xieee32` syntax in lex.fsl (xinteger 'l' 'f'),
    // not C-style `0x1p3lf` hex-exponent floats — F# doesn't have those.
    assert_streams_match("let a = 0xCAFE\nlet b = 0o755\nlet c = 0b1010\nlet d = 0x1lf\n");
}

#[test]
fn diff_char_and_string_forms() {
    assert_streams_match(
        "let a = 'x'\nlet b = '\\n'\nlet c = @\"verb\"\nlet d = \"\"\"triple\"\"\"\n",
    );
}

/// The apostrophe char literal `'''` (unescaped `'` body). FCS's `lex.fsl:305`
/// char body does not exclude the apostrophe, so this is `CHAR`, not a run of
/// type-variable quotes. Also pins the byte form `'''B` and the `char`-in-block-
/// comment skipper (`match_char_literal`), which shares the same body rule.
#[test]
fn diff_apostrophe_char_literal() {
    assert_streams_match("let a = '''\nlet b = '''B\n(* ''' *)\nlet c = 1\n");
}

#[test]
fn diff_keyword_aliases_use_return_yield() {
    assert_streams_match(concat!(
        "let f () = use x = obj in return x\n",
        "let g () = yield 1\n",
    ));
}

#[test]
fn diff_range_operator() {
    assert_streams_match("let xs = [1..10]\nlet ys = [1..^10]\n");
}

// ---- operator-boundary cases ------------------------------------------------
// The lexer's `Op` regex must reproduce FCS's tokenization boundaries: a run
// may not *start* with `:` (it only forms the fixed colon-token set) and a run
// of pure `ignored_op_char`s (`.`, `?`) is never a general operator — the exact
// `Dot`/`DotDot`/`QMark` tokens own those. These exercise the splits that the
// old greedy `[...:]+` regex over-munched into a single `Op`.

/// `:^` — `:` can't start a general operator, so it splits into `COLON` +
/// `INFIX_AT_HAT_OP("^")`. (Was `Op(":^")`.)
#[test]
fn diff_op_colon_then_hat() {
    assert_streams_match("let f (x:^a) = x\n");
}

/// `::!` — `COLON_COLON` (cons) followed by `PREFIX_OP("!")`. (Was `Op("::!")`.)
#[test]
fn diff_op_cons_then_bang() {
    assert_streams_match("let xs = a::!b\n");
}

/// `:+` — `COLON` + `PLUS_MINUS_OP("+")`. (Was `Op(":+")`.)
#[test]
fn diff_op_colon_then_plus() {
    assert_streams_match("let x = a:+b\n");
}

/// `...` — pure dots: `DOT_DOT` + `DOT`, never a general operator.
/// (Was `Op("...")`.)
#[test]
fn diff_op_triple_dot() {
    assert_streams_match("let x = a...b\n");
}

/// Regression guard: an `ignored_op_char` (`.`) *prefixing* a significant char
/// is still one operator — `.+.` → `PLUS_MINUS_OP(".+.")`, `.*` → one op.
#[test]
fn diff_op_dot_prefixed_stays_single() {
    assert_streams_match("let x = a .+. b\nlet y = c .* d\n");
}

/// Regression guard: a trailing `:` is a valid non-leading op_char, so
/// `<:` / `>:` stay single compare operators.
#[test]
fn diff_op_trailing_colon_stays_single() {
    assert_streams_match("let x = a <: b\nlet y = c >: d\n");
}

/// Regression guard: `?` standalone is `QMARK` (dynamic-lookup operator),
/// while `?`-prefixing a significant char (`?+`) stays one operator.
#[test]
fn diff_op_qmark_forms() {
    assert_streams_match("let x = a?b\nlet y = c ?+ d\n");
}

/// `__SOURCE_DIRECTORY__`, `__SOURCE_FILE__`, `__LINE__` are lexed as
/// `KEYWORD_STRING` by FCS (`LexHelpers.fs:434-436`). They look like plain
/// identifiers but get their own token kind so the parser can expand them.
#[test]
fn diff_keyword_strings() {
    assert_streams_match(concat!(
        "let dir = __SOURCE_DIRECTORY__\n",
        "let file = __SOURCE_FILE__\n",
        "let line = __LINE__\n",
    ));
}

/// Byte char literals (`'A'B`) become `UINT8`, not `CHAR`, in FCS
/// (lex.fsl:526-585 → `ServiceLexing.fs:1555`). The Rust lexer's `Char`
/// regex aggregates both forms; the harness must dispatch on the `B` suffix.
#[test]
fn diff_byte_char_literals() {
    assert_streams_match("let a = 'x'\nlet b = 'A'B\nlet c = '\\n'B\n");
}

/// Standalone `'` (type-parameter prefix) maps to `QUOTE`, which FCS
/// collapses into `FSharpTokenKind.RightQuote` (ServiceLexing.fs:1472) — the
/// same kind it uses for closing `@>`. Tests `'T` and `'a` in type-parameter
/// position.
#[test]
fn diff_type_parameter_quote() {
    assert_streams_match("let id<'T> (x: 'T) = x\nlet g<'a> (y: 'a list) = y\n");
}

/// Bare interpolated string `$"hello"` — no fills. FCS surfaces this as a
/// single `String`-kind token spanning the full `$"..."`.
#[test]
fn diff_interp_string_bare() {
    assert_streams_match("let a = $\"hello\"\nlet b = $\"\"\n");
}

/// Single-fill interp string `$"x={e}"`. FCS folds the bracketing `{` and
/// `}` into the surrounding `String` token spans: opener span covers
/// `$"x={`, closer span covers `}"`.
#[test]
fn diff_interp_string_single_fill() {
    assert_streams_match("let b = $\"x={1}\"\nlet c = $\"{1}\"\nlet d = $\"a={1+2}b\"\n");
}

/// `\{` is not a string escape in FCS. The backslash is content and the
/// following `{` still opens an interpolation fill, both in the first fragment
/// and after a previous fill closes.
#[test]
fn diff_interp_string_backslash_open_brace_starts_fill() {
    assert_streams_match("let a = $\"\\{1}\"\nlet b = $\"{1}\\{2}\"\n");
}

/// `$@"` inside a block comment. FCS's `comment` rule (lex.fsl:1794-1857)
/// has no interpolation arm: the `$` is an ordinary comment char and `@"`
/// matches the verbatim string arm, so `(* $@"\" *)` uses verbatim rules
/// (`\` literal, the next `"` closes), the comment terminates, and the
/// following `let y = 2` lexes normally. (The `@$"` spelling instead opens
/// a single-quote string-in-comment — covered by the lexer unit test
/// `block_comment_verbatim_interp_spellings_match_fcs`, which can't run
/// through this harness because it intentionally produces an unterminated
/// comment.)
#[test]
fn diff_block_comment_dollar_at_verbatim() {
    assert_streams_match("(* $@\"\\\" *)\nlet y = 2\n");
}

/// Broader smoke against a real F# source — the bridge tool's own
/// `Program.fs`. Any divergence on a real file is a Rust-lexer bug we need
/// to fix before driving LexFilter on top.
#[test]
fn diff_fcs_dump_program() {
    let project = project_dir();
    let path = project.join("Program.fs");
    let source = std::fs::read_to_string(&path).expect("read Program.fs");
    assert_streams_match_path(&source, &path);
}

fn assert_streams_match_path(source: &str, path: &std::path::Path) {
    let fcs_json = fcs_tokens_raw_batch(path);
    let fcs_tokens = parse_fcs_dump(&fcs_json, source);

    let symbols: HashSet<String> = HashSet::new();
    let rust_tokens: Vec<NormalisedToken> = lex_with_symbols(source, &symbols)
        .filter_map(|(tok, span)| {
            let tok = tok.unwrap_or_else(|e| panic!("rust lex error {e:?} in {}", path.display()));
            if is_trivia(&tok) {
                return None;
            }
            let kind = rust_kind_name(&tok);
            Some(NormalisedToken {
                kind,
                start: span.start,
                end: span.end,
            })
        })
        .collect();

    if rust_tokens != fcs_tokens {
        report_divergence(source, &rust_tokens, &fcs_tokens);
    }
}

/// Lex `source` two ways (our Rust lexer, then FCS) and panic on the first
/// disagreement. Mismatches are reported with both streams' first divergence
/// highlighted in context.
fn assert_streams_match(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create temp .fs file");
    tmp.write_all(source.as_bytes()).expect("write source");

    let fcs_json = fcs_tokens_raw_batch(tmp.path());
    let fcs_tokens = parse_fcs_dump(&fcs_json, source);

    let symbols: HashSet<String> = HashSet::new();
    let rust_tokens: Vec<NormalisedToken> = lex_with_symbols(source, &symbols)
        .filter_map(|(tok, span)| {
            let tok = tok.unwrap_or_else(|e| panic!("rust lex error {e:?} in {source:?}"));
            if is_trivia(&tok) {
                return None;
            }
            let kind = rust_kind_name(&tok);
            Some(NormalisedToken {
                kind,
                start: span.start,
                end: span.end,
            })
        })
        .collect();

    if rust_tokens != fcs_tokens {
        report_divergence(source, &rust_tokens, &fcs_tokens);
    }
}
