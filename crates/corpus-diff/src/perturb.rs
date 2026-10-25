//! A copy of a project's sources with non-ASCII text put where it changes no
//! meaning — the input to the handler differential's perturbed run
//! ([`crate::handler_diff`]).
//!
//! The pinned corpus is almost all ASCII, and on ASCII a byte offset, a UTF-16
//! column and a character count are the same number. An LSP that counted
//! columns in bytes would answer every question on it correctly. So the
//! differential asks its questions a second time, of a copy with two kinds of
//! edit:
//!
//! - **Substitution.** Inside comments and plain string literals, ASCII letters
//!   are replaced by non-ASCII text of the same **UTF-16 width**: a pair of
//!   letters by one astral-plane character (two UTF-16 units, four bytes, one
//!   character), a single letter by a two- or three-byte BMP character (one
//!   unit). Every LSP position in the file is unchanged — the line structure is
//!   untouched and every column is the same number of UTF-16 units — while the
//!   bytes before a use on the same line, and the characters, are not. The
//!   offside rule reads columns in the same units, so layout is unchanged too.
//!   Whatever the server answers at a position, it must answer identically on
//!   the copy: a server counting bytes or characters would not.
//! - **Appended code.** Each implementation file gains a module at its end
//!   whose identifiers are backticked non-ASCII names, used after non-ASCII
//!   comments and strings on the same line. These uses carry non-ASCII inside
//!   the name itself, and their columns genuinely shift; nothing in the
//!   unperturbed project answers for them, so they are graded against the
//!   oracle's verdict on the copy alone.
//!
//! The copy is type-checked by FCS too: that is what licenses the claim that
//! the edits changed no meaning, rather than this module's say-so (see
//! [`crate::handler_diff`]'s inertness check).
//!
//! Strings are left alone where their content can matter to the compiler: a
//! format string (any `%`), an escape (any `\`), a byte string, an
//! interpolated string, and the argument of a `#` directive (`#line` names a
//! file, `#nowarn` a warning).

use borzoi_cst::syntax::{SyntaxKind, SyntaxNode};

/// One replacement of the original text: `len` bytes at `at` become `with`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub at: usize,
    pub len: usize,
    pub with: String,
}

/// A perturbed source text and how it was made from the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Perturbed {
    pub text: String,
    /// The substitutions, in order and non-overlapping. The appended module is
    /// not among them: it is after the end of the original text.
    pub edits: Vec<Edit>,
    /// Where the appended module starts in [`Self::text`], if one was appended.
    pub appended_at: Option<usize>,
}

impl Perturbed {
    /// Where original byte `offset` is in the copy. `offset` must not fall
    /// inside an edit.
    pub fn map(&self, offset: usize) -> usize {
        let mut shift: isize = 0;
        for edit in &self.edits {
            if edit.at + edit.len <= offset {
                shift += edit.with.len() as isize - edit.len as isize;
            } else {
                debug_assert!(offset <= edit.at, "offset {offset} is inside {edit:?}");
                break;
            }
        }
        offset
            .checked_add_signed(shift)
            .expect("an edit never shrinks the text")
    }

    /// Whether some substitution lies on the same line as `offset` (an
    /// original offset), before it.
    pub fn substituted_before_on_line(&self, original: &str, offset: usize) -> bool {
        let line_start = original[..offset].rfind(['\n', '\r']).map_or(0, |i| i + 1);
        self.edits
            .iter()
            .any(|edit| edit.at >= line_start && edit.at + edit.len <= offset)
    }
}

/// The text a substituted run of ASCII letters becomes: pairs of letters as one
/// astral-plane character, singles as BMP characters of two and three bytes,
/// cycling so a long run mixes all three. The UTF-16 width always equals the
/// number of letters replaced.
fn substitute_run(letters: usize) -> String {
    let mut out = String::new();
    let mut remaining = letters;
    let mut phase = 0;
    while remaining > 0 {
        match phase % 3 {
            0 if remaining >= 2 => {
                out.push('𝔸');
                remaining -= 2;
            }
            1 => {
                out.push('中');
                remaining -= 1;
            }
            _ => {
                out.push('é');
                remaining -= 1;
            }
        }
        phase += 1;
    }
    out
}

/// The substitutions inside one token's text, `start` being the token's
/// offset. Only runs of ASCII letters are replaced, so delimiters, quotes and
/// nested-comment markers keep their places.
fn substitutions_in(text: &str, start: usize, edits: &mut Vec<Edit>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let run_start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let with = substitute_run(i - run_start);
            debug_assert_eq!(with.encode_utf16().count(), i - run_start);
            edits.push(Edit {
                at: start + run_start,
                len: i - run_start,
                with,
            });
        } else {
            i += 1;
        }
    }
}

/// Whether a string literal's content is inert: no format specifier, no
/// escape, and not the argument of a `#` directive.
fn inert_string(token: &borzoi_cst::syntax::SyntaxToken) -> bool {
    let text = token.text();
    !text.contains(['%', '\\'])
        && !token
            .parent_ancestors()
            .any(|node| node.kind() == SyntaxKind::HASH_DIRECTIVE_DECL)
}

/// The letters of a token that may be substituted, as a byte range within it:
/// a comment's body and a string's content, delimiters excluded.
fn substitutable_body(token: &borzoi_cst::syntax::SyntaxToken) -> Option<(usize, usize)> {
    let text = token.text();
    let len = text.len();
    match token.kind() {
        SyntaxKind::LINE_COMMENT => Some((2.min(len), len)),
        SyntaxKind::BLOCK_COMMENT if len >= 4 => Some((2, len - 2)),
        SyntaxKind::STRING_LIT if len >= 2 && inert_string(token) => Some((1, len - 1)),
        SyntaxKind::VERBATIM_STRING_LIT if len >= 3 && inert_string(token) => Some((2, len - 1)),
        SyntaxKind::TRIPLE_STRING_LIT if len >= 6 && inert_string(token) => Some((3, len - 3)),
        _ => None,
    }
}

/// The module appended to implementation file number `index` of the project:
/// backticked non-ASCII names, used after non-ASCII comments and strings on
/// their own lines. Unique per file, so two files in one namespace cannot
/// collide.
pub fn appended_module(index: usize) -> String {
    format!(
        "\n\nmodule ``Perturbed𝔸{index}`` =\n    \
         let ``café🦀`` = \"ü🦀\"\n    \
         let ``naïve𝔸`` (``x中`` : int) = (* é🦀 *) ``x中`` + String.length ``café🦀``\n    \
         let ``Ωmega`` = (* 𝔸𝔸 *) ``naïve𝔸`` 1 + ``naïve𝔸`` (List.length [ \"é\"; \"🦀\" ])\n"
    )
}

/// Perturb one source file whose parse tree is `root`. `append` is the index to
/// append a module under, or `None` for a file that must not gain one (a
/// signature, or the file holding the entry point, which must stay last).
pub fn perturb(text: &str, root: &SyntaxNode, append: Option<usize>) -> Perturbed {
    let mut edits = Vec::new();
    for token in root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if let Some((from, to)) = substitutable_body(&token) {
            let start = usize::from(token.text_range().start());
            substitutions_in(&token.text()[from..to], start + from, &mut edits);
        }
    }
    edits.sort_by_key(|edit| edit.at);
    let mut out = String::with_capacity(text.len() + 256);
    let mut copied = 0;
    for edit in &edits {
        out.push_str(&text[copied..edit.at]);
        out.push_str(&edit.with);
        copied = edit.at + edit.len;
    }
    out.push_str(&text[copied..]);
    let appended_at = append.map(|index| {
        let at = out.len();
        out.push_str(&appended_module(index));
        at
    });
    Perturbed {
        text: out,
        edits,
        appended_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perturbed(src: &str) -> Perturbed {
        let parse = borzoi_cst::parser::parse(src);
        perturb(src, &parse.root, None)
    }

    #[test]
    fn a_substituted_run_keeps_its_utf16_width() {
        for letters in 1..12 {
            let run = substitute_run(letters);
            assert_eq!(run.encode_utf16().count(), letters, "{run:?}");
            assert!(!run.is_ascii());
        }
        assert!(substitute_run(5).contains('𝔸'));
        assert!(substitute_run(5).contains('中'));
        assert!(substitute_run(5).contains('é'));
    }

    #[test]
    fn comments_and_inert_strings_are_substituted_and_code_is_not() {
        let src = "let x = \"ab cd\" // see here\nlet y = (* note *) x\nlet z = sprintf \"%d\" 1\nlet w = \"a\\nb\"\n";
        let p = perturbed(src);
        let lines: Vec<&str> = p.text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("let x = \""), "{}", lines[0]);
        assert!(!lines[0].contains("ab"), "{}", lines[0]);
        assert!(!lines[0].contains("see"), "{}", lines[0]);
        assert!(lines[1].starts_with("let y = (* "), "{}", lines[1]);
        assert!(lines[1].ends_with(" *) x"), "{}", lines[1]);
        // A format string and an escaping string are left alone.
        assert_eq!(lines[2], "let z = sprintf \"%d\" 1");
        assert_eq!(lines[3], "let w = \"a\\nb\"");
        // Every column is the same number of UTF-16 units.
        for (a, b) in src.lines().zip(p.text.lines()) {
            assert_eq!(a.encode_utf16().count(), b.encode_utf16().count());
        }
        // Code after a substitution maps to where it now is.
        let x_use = src.rfind("x\n").unwrap();
        assert_eq!(&p.text[p.map(x_use)..p.map(x_use) + 1], "x");
        assert!(p.substituted_before_on_line(src, x_use));
        assert!(!p.substituted_before_on_line(src, src.find("let z").unwrap()));
    }

    #[test]
    fn the_appended_module_is_after_the_original_text() {
        let src = "module M\nlet a = 1\n";
        let parse = borzoi_cst::parser::parse(src);
        let p = perturb(src, &parse.root, Some(3));
        let at = p.appended_at.unwrap();
        assert_eq!(&p.text[..at], src);
        assert!(p.text[at..].contains("``Perturbed𝔸3``"));
    }
}
