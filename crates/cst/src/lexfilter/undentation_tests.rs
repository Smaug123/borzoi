//! Equivalence tests for the cache-accelerated `undentation_limit`.
//!
//! `Filter::undentation_limit` was made linear by jumping over maximal runs of
//! "pure-skip" offside contexts via the `undentation_skip` cache (instead of
//! the O(depth)-per-push stepwise walk). This module holds the **reference
//! oracle** — [`undentation_limit_reference`], a verbatim, jump-free copy of the
//! stepwise walk — and a property test asserting the production (cached) function
//! agrees with it for arbitrary offside stacks, `new_ctxt`, and `strict`.
//!
//! The corpus diff can't catch a deep-walk divergence (real source is shallow),
//! so this generator-driven equivalence check is the real correctness gate for
//! the optimisation. The reference is the proven pre-cache implementation, so a
//! mismatch means the jump (or the skip cache) is wrong.

use super::*;
use proptest::prelude::*;

/// A concrete `Filter` instantiation, only used to name the pure associated
/// functions `undentation_limit` / `is_pure_skip` (neither touches the iterator
/// `I` or the borrow `'a`). Never constructed.
type RefFilter = Filter<'static, std::iter::Empty<(Result<Token<'static>, LexError>, Span)>>;

/// Verbatim, jump-free copy of `Filter::undentation_limit`'s stepwise walk — the
/// behavioural spec the cached version must match. The ONLY difference from
/// production is the absence of the `skip`-cache fast-path, so any divergence
/// localises to that jump.
fn undentation_limit_reference(
    mut strict: bool,
    new_ctxt: &Context,
    mut stack: &[Context],
) -> PositionWithColumn {
    loop {
        let Some((head, rest)) = stack.split_last() else {
            return PositionWithColumn {
                pos: new_ctxt.start_pos(),
                col: -1,
            };
        };

        if let Context::Vanilla { .. } = head {
            stack = rest;
            continue;
        }

        if !strict && matches!(head, Context::SeqBlock { .. } | Context::Paren { .. }) {
            stack = rest;
            continue;
        }

        if let Context::Paren { opener, .. } = head
            && opener.is_token_l_expr_paren()
        {
            strict = false;
            stack = rest;
            continue;
        }
        if let Context::SeqBlock { .. } = head
            && let Some(Context::Paren { opener, .. }) = rest.last()
            && opener.is_token_l_expr_paren()
        {
            strict = false;
            stack = &rest[..rest.len() - 1];
            continue;
        }

        if let Context::Paren {
            opener: Opener::Class | Opener::Struct | Opener::Interface,
            ..
        } = head
            && let [
                ..,
                limit_ctxt @ Context::TypeDefns { .. },
                Context::SeqBlock { .. },
            ] = rest
        {
            return PositionWithColumn {
                pos: limit_ctxt.start_pos(),
                col: (limit_ctxt.start_pos().col as i32) + 1,
            };
        }

        if let Context::SeqBlock { .. } = new_ctxt
            && let Context::Else { .. } = head
            && let Some(Context::If { pos: if_pos }) = rest.last()
        {
            return PositionWithColumn {
                pos: *if_pos,
                col: if_pos.col as i32,
            };
        }

        if let Context::SeqBlock { first: true, .. } = new_ctxt
            && let Context::Do { pos: do_pos } = head
            && let [
                ..,
                Context::TypeDefns { .. } | Context::ModuleBody { .. },
                Context::SeqBlock { .. },
            ] = rest
        {
            return PositionWithColumn {
                pos: *do_pos,
                col: (do_pos.col as i32) + 1,
            };
        }

        if let Context::SeqBlock { first: true, .. } = new_ctxt
            && let Context::WithAsAugment { .. } = head
            && let Some(limit_ctxt @ Context::TypeDefns { .. }) = rest.last()
        {
            return PositionWithColumn {
                pos: limit_ctxt.start_pos(),
                col: (limit_ctxt.start_pos().col as i32) + 1,
            };
        }

        if let Context::WithAsAugment { .. } = new_ctxt
            && matches!(
                head,
                Context::MemberHead { .. }
                    | Context::TypeDefns { .. }
                    | Context::Exception { .. }
                    | Context::InterfaceHead { .. }
            )
        {
            return PositionWithColumn {
                pos: head.start_pos(),
                col: head.start_pos().col as i32,
            };
        }

        if let Context::MatchClauses { .. } = new_ctxt
            && let Context::Function { .. } = head
            && let [
                ..,
                limit_ctxt @ Context::LetDecl { .. },
                Context::SeqBlock { .. },
            ] = rest
        {
            return PositionWithColumn {
                pos: limit_ctxt.start_pos(),
                col: limit_ctxt.start_pos().col as i32,
            };
        }

        if matches!(
            head,
            Context::Fun { .. }
                | Context::Function { .. }
                | Context::Then { .. }
                | Context::Else { .. }
                | Context::Do { .. }
                | Context::WithAsAugment { .. }
        ) {
            strict = false;
            stack = rest;
            continue;
        }

        if let Context::Match { pos: match_pos } = head
            && let [
                ..,
                Context::Paren {
                    pos: paren_pos,
                    opener: Opener::Paren | Opener::Begin,
                },
                Context::SeqBlock { .. },
            ] = rest
        {
            let pos = if match_pos.col <= paren_pos.col {
                *match_pos
            } else {
                *paren_pos
            };
            return PositionWithColumn {
                pos,
                col: pos.col as i32,
            };
        }

        if let Context::MatchClauses { pos: mc_pos, .. } = head
            && let [
                ..,
                Context::Paren {
                    pos: paren_pos,
                    opener: Opener::Paren | Opener::Begin,
                },
                Context::SeqBlock { .. },
                Context::Match { .. },
            ] = rest
        {
            let pos = if mc_pos.col <= paren_pos.col {
                *mc_pos
            } else {
                *paren_pos
            };
            return PositionWithColumn {
                pos,
                col: pos.col as i32,
            };
        }

        if let Context::MatchClauses { .. } = head
            && let Some(limit_ctxt) = rest.last()
            && matches!(limit_ctxt, Context::Try { .. } | Context::Match { .. })
        {
            return PositionWithColumn {
                pos: limit_ctxt.start_pos(),
                col: limit_ctxt.start_pos().col as i32,
            };
        }

        // FCS L971-972: If/Else/Then aligned exactly with an enclosing If.
        if let Context::If { pos: if_pos } = head
            && matches!(
                new_ctxt,
                Context::If { .. } | Context::Else { .. } | Context::Then { .. }
            )
        {
            return PositionWithColumn {
                pos: *if_pos,
                col: if_pos.col as i32,
            };
        }

        // FCS L956: CtxtWithAsLet on a CtxtMemberHead → member.col + 1.
        if let Context::WithAsLet { .. } = head
            && let Some(limit_ctxt @ Context::MemberHead { .. }) = rest.last()
        {
            return PositionWithColumn {
                pos: limit_ctxt.start_pos(),
                col: (limit_ctxt.start_pos().col as i32) + 1,
            };
        }

        let pos = head.start_pos();
        let col = pos.col as i32;
        return match head {
            Context::If { .. }
            | Context::LetDecl { .. }
            | Context::WithAsLet { .. }
            | Context::NamespaceHead { .. }
            | Context::ModuleHead { .. }
            | Context::ModuleBody {
                whole_file: false, ..
            }
            | Context::MemberHead { .. }
            | Context::MemberBody { .. }
            | Context::Exception { .. }
            | Context::InterfaceHead { .. } => PositionWithColumn { pos, col: col + 1 },
            Context::Paren { .. }
            | Context::For { .. }
            | Context::When { .. }
            | Context::While { .. }
            | Context::Match { .. }
            | Context::MatchClauses { .. }
            | Context::SeqBlock { .. }
            | Context::Try { .. }
            | Context::NamespaceBody { .. }
            | Context::ModuleBody {
                whole_file: true, ..
            }
            | Context::TypeDefns { .. } => PositionWithColumn { pos, col },
            Context::Vanilla { .. }
            | Context::Fun { .. }
            | Context::Function { .. }
            | Context::Then { .. }
            | Context::Else { .. }
            | Context::Do { .. }
            | Context::WithAsAugment { .. } => {
                unreachable!("recursive arm should have handled {head:?}")
            }
        };
    }
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
    let cached = RefFilter::undentation_limit(strict, new_ctxt, stack, &skip);
    let reference = undentation_limit_reference(strict, new_ctxt, stack);
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
    /// for any stack / new_ctxt / strict.
    #[test]
    fn undentation_cache_matches_reference(
        stack in stack_strategy(),
        new_ctxt in ctxt_strategy(),
        strict in any::<bool>(),
    ) {
        let skip = build_skip(&stack);
        prop_assert_eq!(
            RefFilter::undentation_limit(strict, &new_ctxt, &stack, &skip),
            undentation_limit_reference(strict, &new_ctxt, &stack),
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
        RefFilter::undentation_limit(true, &new_ctxt, &stack, &skip),
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
