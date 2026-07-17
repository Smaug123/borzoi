fn main() {
    let path = std::env::args().nth(1).unwrap();
    let src = std::fs::read_to_string(&path).unwrap();
    for (tok, span) in borzoi_cst::lexer::lex(&src) {
        if let Err(err) = tok {
            let mut start = span.start.saturating_sub(40);
            while start > 0 && !src.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = (span.end + 40).min(src.len());
            while end < src.len() && !src.is_char_boundary(end) {
                end += 1;
            }
            let snippet: String = src[start..end]
                .chars()
                .map(|c| if c == '\n' { '|' } else { c })
                .collect();
            eprintln!(
                "ERR {:?} at {}..{}: …{}…",
                err, span.start, span.end, snippet
            );
            return;
        }
    }
}
