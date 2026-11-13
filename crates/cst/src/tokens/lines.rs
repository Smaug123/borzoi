//! The newline table, [`LineStarts`] (`docs/token-pipeline-soa-plan.md`, D3).

/// A 1-based line and a 0-based byte column, in [`LineStarts`]'s sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinePos {
    pub line: u32,
    pub col: u32,
}

/// The first byte of every line of one source, in FCS's sense.
///
/// FCS breaks lines at `newline = '\n' | '\r' '\n'` (`lex.fsl:315`), so the
/// byte after every `\n` starts a line, and a lone `\r` starts none. That
/// holds wherever the `\n` sits: FCS's lexbuf counts the breaks inside block
/// comments, strings and inactive `#if` regions too, which no token of the
/// lexer's marks. So the table is computed from the bytes, not from tokens.
///
/// FCS strips a file-start UTF-8 BOM before it counts columns, so line 1
/// starts past one. A `U+FEFF` anywhere else is an ordinary character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineStarts {
    /// `starts[i]` is line `i + 1`'s first byte: past a file-start BOM for
    /// line 1, else one past a `\n`. Strictly ascending, and never empty.
    starts: Vec<u32>,
    /// The source's length.
    len: u32,
}

impl LineStarts {
    /// The table of `source`, in one pass over its bytes.
    ///
    /// # Panics
    ///
    /// If `source` is 4 GiB or longer.
    pub fn new(source: &str) -> Self {
        let len = u32::try_from(source.len()).expect("a source is shorter than 4 GiB");
        let first = if source.starts_with('\u{FEFF}') {
            '\u{FEFF}'.len_utf8() as u32
        } else {
            0
        };
        let mut starts = vec![first];
        // A chunk's `\n`s as a bit mask, which the compiler vectorises, then
        // one push per set bit: most chunks hold no `\n`, and those cost a
        // compare of the whole chunk. In range: every index is below `len`.
        const CHUNK: usize = 32;
        let (chunks, tail) = source.as_bytes().as_chunks::<CHUNK>();
        let mut base = 0u32;
        for chunk in chunks {
            let mut mask = 0u32;
            for (j, &b) in chunk.iter().enumerate() {
                mask |= u32::from(b == b'\n') << j;
            }
            while mask != 0 {
                starts.push(base + mask.trailing_zeros() + 1);
                mask &= mask - 1;
            }
            base += CHUNK as u32;
        }
        for (j, &b) in tail.iter().enumerate() {
            if b == b'\n' {
                starts.push(base + j as u32 + 1);
            }
        }
        LineStarts { starts, len }
    }

    /// How many lines the source has: one more than its `\n`s.
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// The first byte of the 1-based `line`.
    ///
    /// # Panics
    ///
    /// If the source has no line `line`.
    pub fn line_start(&self, line: u32) -> usize {
        match (line as usize)
            .checked_sub(1)
            .and_then(|i| self.starts.get(i))
        {
            Some(&start) => start as usize,
            None => panic!("a {}-line source has no line {line}", self.starts.len()),
        }
    }

    /// A cursor at the start of the source.
    pub fn cursor(&self) -> LineCursor<'_> {
        LineCursor {
            table: self,
            line: 0,
            last: 0,
        }
    }
}

/// A position in a [`LineStarts`] that only moves forward, so finding the
/// line of each of an ascending run of offsets costs one step per line
/// crossed, not a search.
#[derive(Debug, Clone)]
pub struct LineCursor<'a> {
    table: &'a LineStarts,
    /// The 0-based index of the line holding `last`.
    line: usize,
    /// The offset of the last [`Self::advance_to`].
    last: u32,
}

impl<'a> LineCursor<'a> {
    /// Move to byte `offset` and return its line and column. The column
    /// counts from the line's start, so it is 0 for an offset inside a
    /// file-start BOM.
    ///
    /// # Panics
    ///
    /// If `offset` is before the cursor's last offset, or past the end of the
    /// source.
    pub fn advance_to(&mut self, offset: usize) -> LinePos {
        assert!(
            offset <= self.table.len as usize,
            "byte {offset} is past the end of a {}-byte source",
            self.table.len
        );
        // In range: checked above.
        let offset = offset as u32;
        assert!(
            offset >= self.last,
            "the cursor only moves forward: byte {offset} after byte {}",
            self.last
        );
        let starts = &self.table.starts;
        while starts
            .get(self.line + 1)
            .is_some_and(|&next| next <= offset)
        {
            self.line += 1;
        }
        self.last = offset;
        LinePos {
            // In range: there are at most `len + 1` lines.
            line: self.line as u32 + 1,
            col: offset.saturating_sub(starts[self.line]),
        }
    }

    /// The table the cursor walks.
    pub fn table(&self) -> &'a LineStarts {
        self.table
    }
}
