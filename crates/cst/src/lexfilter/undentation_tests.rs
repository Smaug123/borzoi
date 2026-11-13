//! Equivalence tests for the cache-accelerated `undentation_limit`.
//!
//! `Filter::undentation_limit` is made linear by jumping over maximal runs of
//! "pure-skip" offside contexts via the `undentation_skip` cache instead of
//! stepping through them one at a time. Passing `skip: None` turns the jump off,
//! leaving the stepwise walk — the behavioural spec — as the **reference
//! oracle** [`undentation_limit_reference`]; a property test asserts the two
//! agree for arbitrary offside stacks, `new_ctxt`, `strict` and language-version
//! gates.
//!
//! The corpus diff can't catch a deep-walk divergence (real source is shallow),
//! so this generator-driven equivalence check is the real correctness gate for
//! the optimisation: a mismatch means the jump (or the skip cache) is wrong.

use super::*;
use proptest::prelude::*;

/// A concrete `Filter` instantiation, only used to name the pure associated
/// functions `undentation_limit` / `is_pure_skip` (neither touches the
/// borrow `'a`). Never constructed.
type RefFilter = Filter<'static>;

/// Every gate on: the language versions from F# 6.0 up.
const ALL_GATES: UndentationGates = UndentationGates {
    relax_whitespace: true,
    relax_whitespace2: true,
};

/// The stepwise walk: `Filter::undentation_limit` with the skip-cache jump off.
fn undentation_limit_reference(
    gates: UndentationGates,
    strict: bool,
    new_ctxt: &Context,
    stack: &[Context],
) -> PositionWithColumn {
    RefFilter::undentation_limit(gates, strict, new_ctxt, stack, None)
}
/// Rebuild the `undentation_skip` cache for a stand-alone stack, mirroring
/// `Filter::push_undentation_skip` applied in push order.
fn build_skip(stack: &[Context]) -> Vec<u32> {
    let mut skip: Vec<u32> = Vec::with_capacity(stack.len());
    for (i, ctxt) in stack.iter().enumerate() {
        let entry = if RefFilter::is_pure_skip(ctxt) {
            if i == 0 { u32::MAX } else { skip[i - 1] }
        } else {
            i as u32
        };
        skip.push(entry);
    }
    skip
}

fn assert_cache_matches(strict: bool, new_ctxt: &Context, stack: &[Context]) {
    let skip = build_skip(stack);
    let cached = RefFilter::undentation_limit(ALL_GATES, strict, new_ctxt, stack, Some(&skip));
    let reference = undentation_limit_reference(ALL_GATES, strict, new_ctxt, stack);
    assert_eq!(
        cached, reference,
        "cache/reference mismatch: strict={strict} new_ctxt={new_ctxt:?} stack={stack:?}"
    );
}

// ---- generators ---------------------------------------------------------

fn pos_strategy() -> impl Strategy<Value = Pos> {
    // Only `col` decides the limit; keep it small so many contexts share /
    // straddle columns. `line` is carried through to the returned
    // `PositionWithColumn` (it feeds the FS0058 message), so vary it enough
    // that a wrong-context `pos` shows up in the equivalence check.
    (1u32..3, 0u32..12).prop_map(|(line, col)| Pos { line, col })
}

fn opener_strategy() -> impl Strategy<Value = Opener> {
    prop_oneof![
        Just(Opener::Paren),
        Just(Opener::Brace),
        Just(Opener::Brack),
        Just(Opener::BrackBar),
        Just(Opener::BraceBar),
        Just(Opener::Begin),
        Just(Opener::Sig),
        Just(Opener::Class),
        Just(Opener::Struct),
        Just(Opener::Interface),
        Just(Opener::Quote),
        Just(Opener::TyparAngle),
        Just(Opener::InterpFill),
    ]
}

/// One arbitrary `Context`. Fields `undentation_limit` ignores
/// (`add_block_end`, `depth`, `block_let`, `leading_bar`, `is_long_ident_equals`,
/// `prev`, `attrs`, `nested`, `equals_end`) are fixed; the ones it reads
/// (`pos.col`, `Paren.opener`, `SeqBlock.first`, `ModuleBody.whole_file`) vary.
fn ctxt_strategy() -> impl Strategy<Value = Context> {
    prop_oneof![
        (any::<bool>(), pos_strategy()).prop_map(|(first, pos)| Context::SeqBlock {
            first,
            pos,
            add_block_end: AddBlockEnd::Yes,
        }),
        pos_strategy().prop_map(|pos| Context::LetDecl {
            block_let: false,
            pos
        }),
        pos_strategy().prop_map(|pos| Context::For { pos, depth: 0 }),
        pos_strategy().prop_map(|pos| Context::Do { pos }),
        pos_strategy().prop_map(|pos| Context::While { pos, depth: 0 }),
        pos_strategy().prop_map(|pos| Context::Fun { pos, depth: 0 }),
        pos_strategy().prop_map(|pos| Context::Function { pos }),
        pos_strategy().prop_map(|pos| Context::If { pos }),
        pos_strategy().prop_map(|pos| Context::Then { pos }),
        pos_strategy().prop_map(|pos| Context::Else { pos }),
        pos_strategy().prop_map(|pos| Context::Match { pos }),
        pos_strategy().prop_map(|pos| Context::MatchClauses {
            leading_bar: false,
            pos
        }),
        pos_strategy().prop_map(|pos| Context::When { pos }),
        pos_strategy().prop_map(|pos| Context::Vanilla {
            pos,
            is_long_ident_equals: false
        }),
        (pos_strategy(), opener_strategy())
            .prop_map(|(pos, opener)| Context::Paren { pos, opener }),
        pos_strategy().prop_map(|pos| Context::Try { pos }),
        pos_strategy().prop_map(|pos| Context::WithAsLet { pos }),
        pos_strategy().prop_map(|pos| Context::NamespaceHead {
            pos,
            prev: NamespacePrev::Keyword
        }),
        pos_strategy().prop_map(|pos| Context::NamespaceBody { pos }),
        pos_strategy().prop_map(|pos| Context::ModuleHead {
            pos,
            prev: ModuleHeadPrev::Module,
            attrs: false,
            nested: false,
        }),
        (any::<bool>(), pos_strategy())
            .prop_map(|(whole_file, pos)| Context::ModuleBody { pos, whole_file }),
        pos_strategy().prop_map(|pos| Context::TypeDefns {
            pos,
            equals_end: None
        }),
        pos_strategy().prop_map(|pos| Context::MemberHead { pos }),
        pos_strategy().prop_map(|pos| Context::MemberBody { pos }),
        pos_strategy().prop_map(|pos| Context::WithAsAugment { pos }),
        pos_strategy().prop_map(|pos| Context::Exception { pos }),
        pos_strategy().prop_map(|pos| Context::InterfaceHead { pos }),
    ]
}

fn stack_strategy() -> impl Strategy<Value = Vec<Context>> {
    prop::collection::vec(ctxt_strategy(), 0..40)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    /// The cache-accelerated production walk must equal the stepwise reference
    /// for any stack / new_ctxt / strict / gates.
    #[test]
    fn undentation_cache_matches_reference(
        stack in stack_strategy(),
        new_ctxt in ctxt_strategy(),
        strict in any::<bool>(),
        relax_whitespace in any::<bool>(),
        relax_whitespace2 in any::<bool>(),
    ) {
        let gates = UndentationGates { relax_whitespace, relax_whitespace2 };
        let skip = build_skip(&stack);
        prop_assert_eq!(
            RefFilter::undentation_limit(gates, strict, &new_ctxt, &stack, Some(&skip)),
            undentation_limit_reference(gates, strict, &new_ctxt, &stack),
        );
    }
}

// ---- deterministic checks that the jump path is actually exercised -------

/// A long run of pure-skip contexts (the deep-nesting case) over a
/// limit-imposing bottom must resolve to the bottom's limit via the jump.
#[test]
fn deep_pure_skip_run_jumps_to_limit() {
    let mut stack = vec![Context::LetDecl {
        block_let: false,
        pos: Pos { line: 1, col: 0 },
    }];
    for _ in 0..5000 {
        stack.push(Context::Paren {
            pos: Pos { line: 1, col: 7 },
            opener: Opener::Paren,
        });
        stack.push(Context::SeqBlock {
            first: false,
            pos: Pos { line: 1, col: 7 },
            add_block_end: AddBlockEnd::Yes,
        });
    }
    let new_ctxt = Context::Paren {
        pos: Pos { line: 1, col: 7 },
        opener: Opener::Paren,
    };
    // `let` (line 1, col 0) requires col+1 = 1, anchored at the `let`.
    assert_cache_matches(true, &new_ctxt, &stack);
    let skip = build_skip(&stack);
    assert_eq!(
        RefFilter::undentation_limit(ALL_GATES, true, &new_ctxt, &stack, Some(&skip)),
        PositionWithColumn {
            pos: Pos { line: 1, col: 0 },
            col: 1
        }
    );
}

/// A deep pure-skip run with a `new_ctxt`-sensitive `Else :: If` just above the
/// bottom: the jump must stop at the `Else` (not skip past it) so arm L866 fires.
#[test]
fn deep_run_preserves_new_ctxt_arm() {
    let mut stack = vec![
        Context::If {
            pos: Pos { line: 1, col: 0 },
        },
        Context::Else {
            pos: Pos { line: 2, col: 5 },
        },
    ];
    for _ in 0..3000 {
        stack.push(Context::Paren {
            pos: Pos { line: 3, col: 9 },
            opener: Opener::Paren,
        });
        stack.push(Context::SeqBlock {
            first: false,
            pos: Pos { line: 3, col: 9 },
            add_block_end: AddBlockEnd::Yes,
        });
    }
    // Pushing a SeqBlock: the walk skips the deep paren run, reaches `Else` with
    // `If` below → arm returns the `if`'s column (0), not the catch-all `col+1`.
    let new_ctxt = Context::SeqBlock {
        first: false,
        pos: Pos { line: 99, col: 0 },
        add_block_end: AddBlockEnd::Yes,
    };
    assert_cache_matches(false, &new_ctxt, &stack);
}
