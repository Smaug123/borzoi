//! Capture of `#line` directives seen in active branches.
//!
//! Mirrors the *storage* half of FCS's `LineDirectiveStore`
//! (`src/Compiler/SyntaxTree/LexerStore.fs`): a source-ordered record of the
//! `#line` directives the preprocessor actually saw. A later stage uses this
//! to remap diagnostic spans onto the virtual coordinates the directives
//! assert (see `docs/completed/line-directive-remap-plan.md`); this module only
//! *captures* them — the remap query is not implemented yet.
//!
//! Only directives in *active* branches are recorded. A `#line` inside a
//! dead `#if` arm is never seen by the F# compiler and must not take effect,
//! so the [`crate::directives::Driver`] captures at the point where it
//! swallows the trivia directive, gated on the current branch being active.

/// One `#line` directive that took effect in an active branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineDirective {
    /// 0-based line in the generated source on which the directive sits.
    /// Counted with the F# lexer's newline rule (`\r\n | \n | \r`, each a
    /// single break) so it agrees with the LSP's `offset_to_position` — a
    /// `generated_line` recorded here lines up with the line a diagnostic
    /// byte offset converts to downstream.
    pub generated_line: u32,
    /// The virtual line number `N` the directive asserts for the *next*
    /// source line (`generated_line + 1`). Carried verbatim from
    /// [`crate::directives::Directive::Line`] (so `0` if the digit run
    /// overflowed at parse time, mirroring FCS).
    pub virtual_line: u32,
    /// The virtual file the directive named, or `None` for a bare
    /// `#line N` (a same-file line shift).
    pub file: Option<String>,
}

/// Source-ordered record of active `#line` directives. Entries are appended
/// as the [`crate::directives::Driver`] scans, so the internal vector stays
/// sorted by `generated_line` ascending — the invariant [`remap`] relies on.
///
/// [`remap`]: LineDirectiveStore::remap
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineDirectiveStore {
    directives: Vec<LineDirective>,
}

/// The virtual coordinates a [`LineDirectiveStore::remap`] query resolves
/// to: the file the governing directive named (`None` for a same-file
/// shift) and the 0-based virtual line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remapped {
    /// The directive's named file, or `None` for a bare `#line N`.
    pub file: Option<String>,
    /// 0-based virtual line, ready to drop into an LSP `Position.line`.
    pub line: u32,
}

impl LineDirectiveStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a directive. Callers push in source order.
    pub fn push(&mut self, directive: LineDirective) {
        self.directives.push(directive);
    }

    /// The captured directives, in source order.
    pub fn directives(&self) -> &[LineDirective] {
        &self.directives
    }

    /// Whether any directive was captured.
    pub fn is_empty(&self) -> bool {
        self.directives.is_empty()
    }

    /// Map a 0-based generated line to the virtual coordinates asserted by
    /// the most recent preceding `#line` directive, or `None` when no
    /// directive precedes `generated_line` (the caller keeps the generated
    /// coordinates).
    ///
    /// Mirrors FCS's `range.ApplyLineDirectives`: the boundary is strict
    /// (`directive.generated_line < generated_line`) and the *last*
    /// qualifying directive wins. The shifted line is `generated_line +
    /// virtual_line − directive.generated_line − 2` (0-based in, 0-based
    /// out), clamped at 0. The `− 2` reconciles our 0-based `generated_line`
    /// with the 1-based literal `virtual_line`; see
    /// `docs/completed/line-directive-remap-plan.md` for the derivation.
    pub fn remap(&self, generated_line: u32) -> Option<Remapped> {
        let count = self
            .directives
            .partition_point(|d| d.generated_line < generated_line);
        let directive = &self.directives[count.checked_sub(1)?];
        let line = i64::from(generated_line) + i64::from(directive.virtual_line)
            - i64::from(directive.generated_line)
            - 2;
        Some(Remapped {
            file: directive.file.clone(),
            line: line.clamp(0, i64::from(u32::MAX)) as u32,
        })
    }
}

/// 0-based line index of `offset` within `source`, counting line breaks with
/// the F# lexer's rule (`\r\n | \n | \r`, each a single break).
///
/// Kept byte-for-byte consistent with the LSP's `offset_to_position` line
/// counting: the two must agree so that a `generated_line` recorded at
/// capture time matches the line a diagnostic offset converts to when the
/// remap consumer (a later stage) looks the directive up.
pub(crate) fn line_index(source: &str, offset: usize) -> u32 {
    let offset = offset.min(source.len());
    let mut line: u32 = 0;
    let mut chars = source[..offset].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                line += 1;
                // `\r\n` is one break, not two.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            '\n' => line += 1,
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_index_counts_lf() {
        assert_eq!(line_index("a\nb\nc", 0), 0);
        assert_eq!(line_index("a\nb\nc", 2), 1);
        assert_eq!(line_index("a\nb\nc", 4), 2);
    }

    #[test]
    fn line_index_counts_crlf_as_one() {
        // Offset at the start of the second line (`b`) is one break in.
        assert_eq!(line_index("a\r\nb", 3), 1);
    }

    #[test]
    fn line_index_counts_lone_cr() {
        assert_eq!(line_index("a\rb", 2), 1);
        assert_eq!(line_index("a\r\rxy", 3), 2);
    }

    #[test]
    fn line_index_clamps_past_eof() {
        assert_eq!(line_index("a\nb", 100), 1);
    }

    #[test]
    fn store_starts_empty_and_records_in_order() {
        let mut store = LineDirectiveStore::new();
        assert!(store.is_empty());
        store.push(LineDirective {
            generated_line: 0,
            virtual_line: 5,
            file: Some("foo.fsl".to_string()),
        });
        store.push(LineDirective {
            generated_line: 3,
            virtual_line: 10,
            file: None,
        });
        assert!(!store.is_empty());
        assert_eq!(
            store.directives(),
            &[
                LineDirective {
                    generated_line: 0,
                    virtual_line: 5,
                    file: Some("foo.fsl".to_string()),
                },
                LineDirective {
                    generated_line: 3,
                    virtual_line: 10,
                    file: None,
                },
            ]
        );
    }

    // ---- remap --------------------------------------------------------------

    fn ld(generated_line: u32, virtual_line: u32, file: Option<&str>) -> LineDirective {
        LineDirective {
            generated_line,
            virtual_line,
            file: file.map(str::to_string),
        }
    }

    fn store_of(directives: &[LineDirective]) -> LineDirectiveStore {
        let mut store = LineDirectiveStore::new();
        for d in directives {
            store.push(d.clone());
        }
        store
    }

    /// Independent remap: linear find-last-before + `i64` arithmetic + clamp.
    /// Picks the max-`generated_line` directive below the query, so it makes
    /// no sortedness assumption and genuinely cross-checks the
    /// `partition_point`-based production version on sorted stores.
    fn remap_ref(store: &LineDirectiveStore, query: u32) -> Option<Remapped> {
        let directive = store
            .directives()
            .iter()
            .filter(|d| d.generated_line < query)
            .max_by_key(|d| d.generated_line)?;
        let line = i64::from(query) + i64::from(directive.virtual_line)
            - i64::from(directive.generated_line)
            - 2;
        Some(Remapped {
            file: directive.file.clone(),
            line: line.clamp(0, i64::from(u32::MAX)) as u32,
        })
    }

    #[test]
    fn remap_on_directive_line_or_before_is_none() {
        let store = store_of(&[ld(2, 100, Some("a.fs"))]);
        // Strict boundary: the directive's own line and earlier keep
        // generated coordinates.
        assert_eq!(store.remap(0), None);
        assert_eq!(store.remap(2), None);
    }

    #[test]
    fn remap_line_after_directive_pins_off_by_one() {
        let store = store_of(&[ld(0, 100, Some("a.fs"))]);
        // `#line 100` on generated line 0: the next line (1) displays as
        // 100, i.e. 0-based 99.
        assert_eq!(
            store.remap(1),
            Some(Remapped {
                file: Some("a.fs".into()),
                line: 99
            })
        );
        assert_eq!(
            store.remap(2),
            Some(Remapped {
                file: Some("a.fs".into()),
                line: 100
            })
        );
        assert_eq!(
            store.remap(5),
            Some(Remapped {
                file: Some("a.fs".into()),
                line: 103
            })
        );
    }

    #[test]
    fn remap_uses_most_recent_directive() {
        let store = store_of(&[ld(0, 100, Some("a.fs")), ld(5, 200, Some("b.fs"))]);
        // Between the two, the first governs.
        assert_eq!(
            store.remap(3),
            Some(Remapped {
                file: Some("a.fs".into()),
                line: 101
            })
        );
        // After the second, the second governs.
        assert_eq!(
            store.remap(7),
            Some(Remapped {
                file: Some("b.fs".into()),
                line: 200
            })
        );
    }

    #[test]
    fn remap_carries_none_file_for_bare_directive() {
        let store = store_of(&[ld(2, 50, None)]);
        assert_eq!(
            store.remap(3),
            Some(Remapped {
                file: None,
                line: 49
            })
        );
    }

    #[test]
    fn remap_clamps_line_zero_directive() {
        let store = store_of(&[ld(0, 0, None)]);
        // `#line 0` on line 0: query 1 → 1 + 0 − 0 − 2 = −1 → clamp 0.
        assert_eq!(
            store.remap(1),
            Some(Remapped {
                file: None,
                line: 0
            })
        );
    }

    /// An ascending store (the real invariant) paired with a query, for the
    /// equivalence and boundary properties.
    fn arb_store() -> impl proptest::strategy::Strategy<Value = LineDirectiveStore> {
        use proptest::prelude::*;
        prop::collection::vec(
            (
                1u32..50,
                0u32..1000,
                prop::option::of(prop_oneof![
                    Just("a.fs".to_string()),
                    Just("b.fs".to_string())
                ]),
            ),
            0..20,
        )
        .prop_map(|rows| {
            let mut store = LineDirectiveStore::new();
            let mut generated_line = 0u32;
            for (gap, virtual_line, file) in rows {
                store.push(LineDirective {
                    generated_line,
                    virtual_line,
                    file,
                });
                generated_line = generated_line.saturating_add(gap);
            }
            store
        })
    }

    proptest::proptest! {
        /// `remap` agrees with the independent linear oracle for all queries.
        #[test]
        fn remap_matches_reference(store in arb_store(), query in 0u32..2000) {
            proptest::prop_assert_eq!(store.remap(query), remap_ref(&store, query));
        }

        /// `remap` is `Some` exactly when some directive precedes the query.
        #[test]
        fn remap_none_iff_no_directive_precedes(store in arb_store(), query in 0u32..2000) {
            let any_before = store.directives().iter().any(|d| d.generated_line < query);
            proptest::prop_assert_eq!(store.remap(query).is_some(), any_before);
        }
    }
}
