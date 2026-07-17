//! The diff harness's token-kind namer must be TOTAL over every operator
//! string the lexer's `Op` regex (`[!$%&*+\-./<=>?@^|~:]+`) can produce.
//!
//! The lexer over-munches `:`- and `.`-led operator runs into a single
//! `Token::Op` (e.g. `::!`, `:^`, `...`) that FCS would split into structural
//! tokens (`ColonColon`+`PrefixOp`, `Colon`+`InfixAtHatOp`, `DotDot`+`Dot`).
//! `op_kind_name` has no single FCS kind for these, but it must still return
//! *some* name so the corpus sweep records the divergence in its histogram
//! instead of panicking and dropping the whole file.

use crate::common::filtered_kind_name;
use borzoi_cst::lexer::Token;
use borzoi_cst::lexfilter::FilteredToken;
use proptest::prelude::*;

/// The three over-munched operators observed in the F# corpus
/// sweep (counts: `:^` ×10, `::!` ×1, `...` ×1); each previously panicked the
/// namer at `common::op_kind_name`.
#[test]
fn over_munched_operators_do_not_panic() {
    for op in ["::!", ":^", "..."] {
        let name = filtered_kind_name(&FilteredToken::Raw(Token::Op(op)));
        assert!(!name.is_empty(), "{op:?} produced an empty kind name");
    }
}

proptest! {
    /// Totality: no string over the lexer's `Op` alphabet may panic the namer.
    #[test]
    fn kind_name_total_over_op_alphabet(s in "[!$%&*+\\-./<=>?@^|~:]+") {
        let _ = filtered_kind_name(&FilteredToken::Raw(Token::Op(&s)));
    }
}
