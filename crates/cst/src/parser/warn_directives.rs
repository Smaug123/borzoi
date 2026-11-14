//! The diagnostics FCS reports for the `#nowarn` / `#warnon` directives of a
//! file (`SyntaxTree/WarnScopes.fs`): while lexing each directive
//! (`parseDirective`), and once the file is parsed, as it turns them into
//! warning scopes (`MergeInto`).
//!
//! Two language features move them. Below
//! [`LanguageFeature::ParsedHashDirectiveArgumentNonQuotes`] (9.0) an
//! unquoted argument is FS3350 and an `FS`-prefixed one FS0203; below
//! [`LanguageFeature::ScopedNowarn`] (10.0) a top-level `#warnon` is FS3350, a
//! directive inside a nested module is FS0236, and an argument that is not a
//! number is FS0203 as a warning (from 9.0) or not at all (below), where from
//! 10.0 it is an error. Both features' effects are `Diagnose+Severity`, so every
//! diagnostic here is [`DiagnosticClass::Recovery`](super::DiagnosticClass).
//!
//! Not reported here: FCS's informational FS3876 for a `#nowarn` that repeats
//! an open one (a parse reports errors and warnings only). FS3874, for a
//! directive that does not start its line, is the preprocessor's
//! (`PreprocError::WarnDirectiveNotFirst`); such a directive is read here like
//! any other, as FCS registers it.

use std::ops::Range;

use crate::language_version::{LanguageFeature, LanguageVersion};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use super::{ParseDiagnostic, ParseError};

/// The diagnostics, errors then warnings, FCS reports for the warn directives
/// of the tree `root`, parsed from `source` at `lang`.
pub(super) fn warn_directive_diagnostics(
    root: &SyntaxNode,
    source: &str,
    lang: LanguageVersion,
) -> (Vec<ParseDiagnostic>, Vec<ParseDiagnostic>) {
    let mut report = Report {
        lang,
        errors: Vec::new(),
        warnings: Vec::new(),
    };
    let directives: Vec<WarnDirective> = root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|t| t.kind() == SyntaxKind::WARN_DIRECTIVE)
        .map(|t| {
            let start = usize::from(t.text_range().start());
            parse_directive(t.text(), start, &mut report)
        })
        .collect();
    if directives.is_empty() {
        return (report.errors, report.warnings);
    }
    let commands: Vec<WarnCmd> = if lang.supports(LanguageFeature::ScopedNowarn) {
        directives.into_iter().flat_map(|d| d.commands).collect()
    } else {
        let submodules = submodule_ranges(root);
        let mut commands = Vec::new();
        for directive in directives {
            let range = &directive.range;
            // The submodules are disjoint and in source order, so only the last
            // to start at or before the directive can hold it.
            let before = submodules.partition_point(|m| m.start <= range.start);
            if before > 0 && range.end <= submodules[before - 1].end {
                report.warning(
                    "Directives inside modules are ignored".to_string(),
                    range.clone(),
                );
            } else if directive.commands.first().is_some_and(|c| c.warnon) {
                report.gate(LanguageFeature::ScopedNowarn, range.clone());
            } else {
                commands.extend(directive.commands);
            }
        }
        commands
    };
    report_unmatched_warnons(source, &commands, &mut report);
    (report.errors, report.warnings)
}

struct Report {
    lang: LanguageVersion,
    errors: Vec<ParseDiagnostic>,
    warnings: Vec<ParseDiagnostic>,
}

impl Report {
    fn error(&mut self, message: String, span: Range<usize>) {
        self.errors.push(ParseError { message, span }.recovery());
    }

    fn warning(&mut self, message: String, span: Range<usize>) {
        self.warnings.push(ParseError { message, span }.recovery());
    }

    fn gate(&mut self, feature: LanguageFeature, span: Range<usize>) {
        self.errors.push(ParseDiagnostic::gate(
            feature,
            feature.not_available_message(self.lang),
            span,
        ));
    }
}

/// One `#nowarn` / `#warnon` directive, as FCS's `parseDirective` reads it.
struct WarnDirective {
    /// FCS's `DirectiveRange`.
    range: Range<usize>,
    /// The arguments it reads as warning numbers, in order.
    commands: Vec<WarnCmd>,
}

/// FCS's `WarnCmd`: one warning number a directive turns off (`#nowarn`) or
/// on (`#warnon`), at the range of its argument.
struct WarnCmd {
    warnon: bool,
    number: i32,
    span: Range<usize>,
}

/// FCS's `parseDirective` over the directive token `text` starting at byte
/// `start`, reporting what it reports into `report`.
fn parse_directive(text: &str, start: usize, report: &mut Report) -> WarnDirective {
    let lang = report.lang;
    let matched = match_directive(text);
    // FCS's `DirectiveRange`: offset from the token's start by the spaces its
    // regex's first group takes before the `#`, and as long as the match less
    // those spaces. The match runs to the end of the line, its comment
    // included, so for a directive whose spaces start its line the range runs
    // from the `#` to the end of the line. With no match it is empty, at the
    // token's start. FCS counts these in UTF-16 columns from the token's
    // start, so they are counted so here and read back as bytes.
    let range = match &matched {
        Some(m) => {
            let match_start = m.hash - m.indentation;
            let units = text[match_start..m.end].encode_utf16().count();
            start + at_utf16(text, m.indentation)..start + at_utf16(text, units)
        }
        None => start..start,
    };
    let (ident, args) = match &matched {
        Some(m) => (&text[m.ident.clone()], m.args.as_slice()),
        None => ("", &[][..]),
    };
    if args.is_empty() {
        report.error(
            "Warn directives must have warning number(s) as argument(s)".to_string(),
            range.clone(),
        );
    }
    let warnon = match ident {
        "warnon" => true,
        "nowarn" => false,
        _ => {
            report.error(format!("Invalid directive '##{ident} '"), range.clone());
            return WarnDirective {
                range,
                commands: Vec::new(),
            };
        }
    };
    let commands = args
        .iter()
        .filter_map(|arg| {
            let span = start + arg.start..start + arg.end;
            let number = warning_number(&text[arg.clone()], span.clone(), lang, report)?;
            Some(WarnCmd {
                warnon,
                number,
                span,
            })
        })
        .collect();
    WarnDirective { range, commands }
}

/// The byte offset in `text` of its UTF-16 column `units`, rounded up to a
/// character and clamped to the text's end.
fn at_utf16(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (i, c) in text.char_indices() {
        if seen >= units {
            return i;
        }
        seen += c.len_utf16();
    }
    text.len()
}

/// FCS's `getNumber`: the warning number one argument names, if it names one
/// at `lang`, reporting why not otherwise.
fn warning_number(
    arg: &str,
    span: Range<usize>,
    lang: LanguageVersion,
    report: &mut Report,
) -> Option<i32> {
    let non_quotes = LanguageFeature::ParsedHashDirectiveArgumentNonQuotes;
    // `removeQuotes`. An argument of a lone `"` both starts and ends with one,
    // and FCS then takes a substring of negative length; it is read here as
    // the empty string.
    let unquoted = if arg.starts_with('"') && arg.ends_with('"') {
        if arg.len() >= 6 && arg.starts_with("\"\"\"") && arg.ends_with("\"\"\"") {
            &arg[3..arg.len() - 3]
        } else {
            arg.get(1..arg.len() - 1).unwrap_or("")
        }
    } else if lang.supports(non_quotes) {
        arg
    } else {
        report.gate(non_quotes, span);
        return None;
    };
    // `removePrefix`.
    let digits = match unquoted.strip_prefix("FS") {
        Some(digits) if lang.supports(non_quotes) => digits,
        Some(_) => {
            report.warning(format!("Invalid warning number '{unquoted}'"), span);
            return None;
        }
        None => unquoted,
    };
    // `parseInt`: `Int32.TryParse`, whose default style (`NumberStyles.Integer`)
    // admits a leading sign and surrounding white space, U+0009 to U+000D and
    // U+0020; of those, an argument can hold a tab, a vertical tab and a form
    // feed.
    // It also admits NUL characters after the number and its white space.
    let is_white = |c: char| matches!(c, '\u{9}'..='\u{d}' | ' ');
    let trimmed = digits.trim_end_matches('\0').trim_matches(is_white);
    match trimmed.parse::<i32>() {
        Ok(number) => Some(number),
        Err(_) => {
            let message = format!("Invalid warning number '{unquoted}'");
            if lang.supports(LanguageFeature::ScopedNowarn) {
                report.error(message, span);
            } else if lang.supports(non_quotes) {
                report.warning(message, span);
            }
            None
        }
    }
}

/// The ranges of the nested modules a file's top-level modules and namespaces
/// declare (FCS's `getImplSubmoduleRanges` / `getSpecSubmoduleRanges`), from
/// the first source token to the last.
fn submodule_ranges(root: &SyntaxNode) -> Vec<Range<usize>> {
    root.children()
        .filter(|n| n.kind() == SyntaxKind::MODULE_OR_NAMESPACE)
        .flat_map(|m| m.children())
        .filter(|n| n.kind() == SyntaxKind::NESTED_MODULE_DECL)
        .filter_map(|n| {
            let mut tokens = n
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|t| !t.kind().is_trivia() && !t.text().is_empty());
            let first = tokens.next()?;
            let last = tokens.last().unwrap_or_else(|| first.clone());
            Some(usize::from(first.text_range().start())..usize::from(last.text_range().end()))
        })
        .collect()
}

/// FCS's `processWarnCmd` fold, for the one diagnostic it reports that is not
/// informational: a `#warnon` of a warning already on.
fn report_unmatched_warnons(source: &str, commands: &[WarnCmd], report: &mut Report) {
    // Per warning number, FCS's scope stack: its top, and the range FCS
    // quotes the line of.
    #[derive(Clone)]
    enum Scope {
        /// `#nowarn`, not yet closed.
        OpenOff,
        /// `#warnon` closing a `#nowarn`.
        Off,
        /// `#warnon`, not yet closed.
        OpenOn,
        /// `#nowarn` closing a `#warnon`.
        On,
    }
    let mut tops: std::collections::HashMap<i32, (Scope, Range<usize>)> =
        std::collections::HashMap::new();
    // The start of each line, read once, for the 1-based line FCS quotes.
    let mut line_starts: Option<Vec<usize>> = None;
    for cmd in commands {
        let top = tops.get(&cmd.number).cloned();
        let next = match (cmd.warnon, top) {
            (false, Some((Scope::OpenOn, open))) => Some((Scope::On, open.start..cmd.span.end)),
            // FCS reports an informational FS3876 here from 10.0 on.
            (false, Some((Scope::OpenOff | Scope::On, _))) => None,
            (false, _) => Some((Scope::OpenOff, cmd.span.clone())),
            (true, Some((Scope::OpenOff, open))) => Some((Scope::Off, open.start..cmd.span.end)),
            (true, Some((Scope::OpenOn | Scope::Off, open))) => {
                let starts = line_starts.get_or_insert_with(|| {
                    std::iter::once(0)
                        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
                        .collect()
                });
                let line = starts.partition_point(|&s| s <= open.end);
                report.warning(
                    format!("There is another #warnon for this warning already in line {line}."),
                    cmd.span.clone(),
                );
                None
            }
            (true, _) => Some((Scope::OpenOn, cmd.span.clone())),
        };
        if let Some(scope) = next {
            tops.insert(cmd.number, scope);
        }
    }
}

/// Where FCS's directive regex, `( *)#(\S+)(?: +([^ \r\n/;]+))*(?:;;)?( *)(\/\/.*)?$`,
/// matches a directive token: byte offsets into the token's text.
#[derive(Debug, PartialEq, Eq)]
struct DirectiveMatch {
    /// The `#`.
    hash: usize,
    /// How many spaces the match takes before the `#`.
    indentation: usize,
    /// The identifier after the `#` (`nowarn`).
    ident: Range<usize>,
    /// The arguments.
    args: Vec<Range<usize>>,
    /// The end of the match.
    end: usize,
}

/// The leftmost, backtracking match of FCS's directive regex in `text`, as
/// .NET's `Regex.Match` finds it, in time linear in `text`.
///
/// An argument and a run of spaces between arguments never give back a byte
/// to what follows them: what could follow is a space, `;;` or `//`, none of
/// which an argument holds, or an argument, which never starts with a space.
/// So from any position the repetition of arguments has one chain of
/// boundaries, and the only choices are how far the identifier runs and how
/// many arguments the repetition takes, each tried longest first. The chain
/// from a boundary is the tail of the chain from any earlier one, so which
/// boundary of it the match ends at is computed once for every position.
fn match_directive(text: &str) -> Option<DirectiveMatch> {
    // FCS's lexeme stops before the line break.
    let text = text.strip_suffix('\r').unwrap_or(text);
    let bytes = text.as_bytes();
    let n = bytes.len();
    // The first byte at or after each position that is not a space.
    let mut past_spaces = vec![n; n + 1];
    for q in (0..n).rev() {
        past_spaces[q] = if bytes[q] == b' ' {
            past_spaces[q + 1]
        } else {
            q
        };
    }
    // `(?:;;)?( *)(\/\/.*)?$`, the optional `;;` tried first. `$` with no
    // `Multiline` matches at the end, or before a final `\n`, which a
    // directive token never holds; `.` matches anything but `\n`.
    let rest = |q: usize| {
        let r = past_spaces[q];
        r == n || bytes[r..].starts_with(b"//")
    };
    let tail_matches_at = |q: usize| (bytes[q..].starts_with(b";;") && rest(q + 2)) || rest(q);
    // The end of the run of argument bytes (`[^ \r\n/;]`) starting at each
    // position.
    let is_arg = |b: u8| !matches!(b, b' ' | b'\r' | b'\n' | b'/' | b';');
    let mut arg_end = vec![n; n + 1];
    for q in (0..n).rev() {
        arg_end[q] = if is_arg(bytes[q]) { arg_end[q + 1] } else { q };
    }
    // One more ` +A` from boundary `q`: the argument it takes, if any.
    let next_arg = |q: usize| -> Option<Range<usize>> {
        if bytes.get(q) != Some(&b' ') {
            return None;
        }
        let a = past_spaces[q];
        (a < n && is_arg(bytes[a])).then(|| a..arg_end[a])
    };
    // Where the repetition from each position ends when the rest matches
    // after it, taking as many arguments as will go.
    let mut chain_end: Vec<Option<usize>> = vec![None; n + 1];
    for q in (0..=n).rev() {
        chain_end[q] = next_arg(q)
            .and_then(|arg| chain_end[arg.end])
            .or(tail_matches_at(q).then_some(q));
    }
    let mut resume = 0;
    for (hash, _) in text.match_indices('#') {
        // A `#` inside the identifier of a candidate that failed fails too:
        // its identifier ends where that one's did, and the shorter ones it
        // could take were tried.
        if hash < resume {
            continue;
        }
        let indentation = text[..hash]
            .bytes()
            .rev()
            .take_while(|&b| b == b' ')
            .count();
        // `\S+`: .NET's `\s` is Unicode white space.
        let ident_start = hash + 1;
        let ident_max = text[ident_start..]
            .find(char::is_whitespace)
            .map_or(n, |i| ident_start + i);
        resume = ident_max;
        if ident_max == ident_start {
            continue;
        }
        if let Some(end) = chain_end[ident_max] {
            let mut args = Vec::new();
            let mut q = ident_max;
            while q < end {
                let arg = next_arg(q).expect("the chain reaches its end");
                q = arg.end;
                args.push(arg);
            }
            return Some(DirectiveMatch {
                hash,
                indentation,
                ident: ident_start..ident_max,
                args,
                end: n,
            });
        }
        // A shorter identifier leaves a non-space byte after it, which only
        // `;;` or `//` can start.
        for ident_end in (ident_start + 1..ident_max).rev() {
            if text.is_char_boundary(ident_end) && tail_matches_at(ident_end) {
                return Some(DirectiveMatch {
                    hash,
                    indentation,
                    ident: ident_start..ident_end,
                    args: Vec::new(),
                    end: n,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(text: &'a str, m: &DirectiveMatch) -> Vec<&'a str> {
        m.args.iter().map(|a| &text[a.clone()]).collect()
    }

    #[test]
    fn a_directive_matches_with_its_arguments() {
        let text = "  #nowarn \"40\" 41 // c";
        let m = match_directive(text).expect("matches");
        assert_eq!((m.hash, &text[m.ident.clone()]), (2, "nowarn"));
        assert_eq!(args(text, &m), ["\"40\"", "41"]);
        assert_eq!(m.end, text.len());
    }

    #[test]
    fn a_double_semicolon_may_follow_the_arguments() {
        let text = "#nowarn \"44\";;";
        let m = match_directive(text).expect("matches");
        assert_eq!(args(text, &m), ["\"44\""]);
    }

    #[test]
    fn the_identifier_runs_to_the_first_white_space() {
        let text = "#nowarn;; \"43\"";
        let m = match_directive(text).expect("matches");
        assert_eq!(&text[m.ident.clone()], "nowarn;;");
        assert_eq!(args(text, &m), ["\"43\""]);
    }

    #[test]
    fn a_glued_comment_is_part_of_the_identifier_but_not_of_an_argument() {
        let text = "#nowarn// c";
        let m = match_directive(text).expect("matches");
        assert_eq!(&text[m.ident.clone()], "nowarn//");
        assert_eq!(args(text, &m), ["c"]);
        let text = "#nowarn \"40\"//c";
        let m = match_directive(text).expect("matches");
        assert_eq!(args(text, &m), ["\"40\""]);
    }

    /// Matching is linear in the directive: a long run of `#`s, or of
    /// arguments, before a `/` that defeats every candidate is read once.
    #[test]
    fn a_defeated_match_is_found_in_linear_time() {
        let hashes = format!("#nowarn {} /", "#".repeat(200_000));
        assert_eq!(match_directive(&hashes), None);
        let args = format!("#nowarn {}/", "#a ".repeat(100_000));
        assert_eq!(match_directive(&args), None);
        let spaced = format!("#nowarn {} x/", " ".repeat(200_000));
        assert_eq!(match_directive(&spaced), None);
    }

    #[test]
    fn a_tab_or_a_bare_semicolon_or_slash_defeats_the_match() {
        for text in ["  #warnon\t\"42\"", "#nowarn 45;46", "#nowarn x/y"] {
            assert_eq!(match_directive(text), None, "{text:?}");
        }
    }
}

/// [`match_directive`] against a direct backtracking reading of the regex,
/// over short directives drawn from the bytes the regex distinguishes.
#[cfg(test)]
mod reference {
    use super::*;
    use proptest::prelude::*;

    /// What the regex captures: the `#`, the spaces before it, the identifier
    /// and the arguments.
    type Captured = (usize, usize, Range<usize>, Vec<Range<usize>>);

    fn run(t: &[u8], from: usize, pred: impl Fn(u8) -> bool) -> usize {
        from + t[from..].iter().take_while(|&&b| pred(b)).count()
    }

    fn is_arg(b: u8) -> bool {
        !matches!(b, b' ' | b'\r' | b'\n' | b'/' | b';')
    }

    /// `(?:;;)?( *)(\/\/.*)?$` at `q`, every quantifier greedy.
    fn tail(t: &[u8], q: usize) -> bool {
        let starts = [t[q..].starts_with(b";;").then_some(q + 2), Some(q)];
        starts.into_iter().flatten().any(|x| {
            (x..=run(t, x, |b| b == b' '))
                .rev()
                .any(|s| t[s..].starts_with(b"//") || s == t.len())
        })
    }

    /// `(?: +([^ \r\n/;]+))*` then the tail, from `q`, more iterations first.
    fn repeat(t: &[u8], q: usize, args: &mut Vec<Range<usize>>) -> bool {
        if t.get(q) == Some(&b' ') {
            for a in (q + 1..=run(t, q, |b| b == b' ')).rev() {
                if !t.get(a).is_some_and(|&b| is_arg(b)) {
                    continue;
                }
                for e in (a + 1..=run(t, a, is_arg)).rev() {
                    args.push(a..e);
                    if repeat(t, e, args) {
                        return true;
                    }
                    args.pop();
                }
            }
        }
        tail(t, q)
    }

    /// The leftmost match, every quantifier greedy, backtracking.
    fn reference(text: &str) -> Option<Captured> {
        let t = text.as_bytes();
        for p in 0..=t.len() {
            for h in (p..=run(t, p, |b| b == b' ')).rev() {
                if t.get(h) != Some(&b'#') {
                    continue;
                }
                let ident_max = run(t, h + 1, |b| !b.is_ascii_whitespace());
                for ie in (h + 2..=ident_max).rev() {
                    let mut args = Vec::new();
                    if repeat(t, ie, &mut args) {
                        return Some((h, h - p, h + 1..ie, args));
                    }
                }
            }
        }
        None
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]
        #[test]
        fn match_directive_is_the_regex(text in "[ #a;/\t\"4]{0,12}") {
            let ours = match_directive(&text).map(|m| (m.hash, m.indentation, m.ident, m.args));
            prop_assert_eq!(ours, reference(&text), "{:?}", text);
        }
    }
}
