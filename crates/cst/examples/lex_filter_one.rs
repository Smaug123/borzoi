use borzoi_cst::lexer::lex;
use borzoi_cst::lexfilter::filter;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: lex_filter_one <path>");
    let src = std::fs::read_to_string(&path).expect("read file");
    for (tok, span) in filter(&src, lex(&src)) {
        println!("{:?} [{}..{})", tok, span.start, span.end);
    }
}
