//! Source positions shared by every compiler stage.

use std::rc::Rc;

/// Half-open byte range `[start, end)` into the module source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const DUMMY: Span = Span { start: 0, end: 0 };

    pub fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }

    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}

/// 1-based line and 1-based column. Columns count UTF-16 code units, the
/// unit used by JavaScript and by the Debug Adapter Protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

impl std::fmt::Display for LineCol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// Maps byte offsets to line/column positions.
#[derive(Clone, Debug)]
pub struct LineIndex {
    src: Rc<str>,
    line_starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(src: Rc<str>) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        LineIndex { src, line_starts }
    }

    pub fn line_col(&self, offset: u32) -> LineCol {
        let off = (offset as usize).min(self.src.len()) as u32;
        let line = match self.line_starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[line] as usize;
        let col = match self.src.get(start..off as usize) {
            Some(s) => s.encode_utf16().count() as u32,
            None => off - start as u32,
        };
        LineCol { line: line as u32 + 1, col: col + 1 }
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Text of a 1-based line without its terminator.
    pub fn line_text(&self, line: u32) -> &str {
        let i = (line as usize).saturating_sub(1);
        let Some(&start) = self.line_starts.get(i) else { return "" };
        let end = self.line_starts.get(i + 1).map(|&e| e as usize).unwrap_or(self.src.len());
        self.src[start as usize..end].trim_end_matches(['\n', '\r'])
    }

    pub fn source(&self) -> &str {
        &self.src
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_utf16() {
        let idx = LineIndex::new("ab\nc😀d\n".into());
        assert_eq!(idx.line_col(0), LineCol { line: 1, col: 1 });
        assert_eq!(idx.line_col(3), LineCol { line: 2, col: 1 });
        // '😀' is 4 UTF-8 bytes and 2 UTF-16 units: 'd' sits at column 4.
        assert_eq!(idx.line_col(8), LineCol { line: 2, col: 4 });
        assert_eq!(idx.line_text(2), "c😀d");
    }
}
