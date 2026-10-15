//! Bounds glyph shaping while retaining full-line Unicode scalar coordinates.

// Rust guideline compliant 2026-02-21
use std::ops::Range;
use unicode_segmentation::GraphemeCursor;

// A fixed rendering budget prevents minified files from creating millions of
// glyphs and hit-test coordinates. Editing and clipboard data remain complete.
pub(super) const MAX_COLUMNS: usize = 256;
// Keep the caret near the leading edge even in narrow document rows.
const LEADING_CONTEXT: usize = 8;

#[derive(Debug, Clone)]
pub(super) struct Viewport {
    pub bytes: Range<usize>,
    pub first_column: usize,
}

impl Viewport {
    pub fn new(text: &str, cursor: usize) -> Self {
        if text.chars().take(MAX_COLUMNS + 1).count() <= MAX_COLUMNS {
            return Self {
                bytes: 0..text.len(),
                first_column: 0,
            };
        }
        let mut first_column = cursor.saturating_sub(LEADING_CONTEXT);
        let mut start = text.char_indices().nth(first_column).map_or_else(
            || {
                first_column = text.chars().count().saturating_sub(LEADING_CONTEXT);
                text.char_indices()
                    .rev()
                    .nth(LEADING_CONTEXT - 1)
                    .map_or(0, |(byte, _)| byte)
            },
            |(byte, _)| byte,
        );
        let cursor_byte = text[start..]
            .char_indices()
            .nth(cursor.saturating_sub(first_column))
            .map_or(text.len(), |(byte, _)| start + byte);
        let mut boundary = GraphemeCursor::new(start, text.len(), true);
        if !boundary
            .is_boundary(text, 0)
            .expect("full text supplies grapheme context")
        {
            if let Some(next) = boundary
                .next_boundary(text, 0)
                .expect("full text supplies grapheme context")
                && next <= cursor_byte
            {
                first_column += text[start..next].chars().count();
                start = next;
            } else {
                let mut boundary = GraphemeCursor::new(start, text.len(), true);
                if let Some(previous) = boundary
                    .prev_boundary(text, 0)
                    .expect("full text supplies grapheme context")
                {
                    let extra = text[previous..start]
                        .chars()
                        .take(LEADING_CONTEXT + 1)
                        .count();
                    if extra <= LEADING_CONTEXT {
                        first_column -= extra;
                        start = previous;
                    }
                }
            }
        }
        let mut end = text[start..]
            .char_indices()
            .nth(MAX_COLUMNS)
            .map_or(text.len(), |(byte, _)| start + byte);
        let mut boundary = GraphemeCursor::new(end, text.len(), true);
        if !boundary
            .is_boundary(text, 0)
            .expect("full text supplies grapheme context")
            && let Some(previous) = boundary
                .prev_boundary(text, 0)
                .expect("full text supplies grapheme context")
            && previous >= cursor_byte
            && previous > start
        {
            end = previous;
        }
        // A single pathological grapheme can exceed the whole rendering budget.
        // In that case retain a scalar-bounded excerpt rather than shaping it all.
        Self {
            bytes: start..end,
            first_column,
        }
    }

    pub fn local(&self, column: usize) -> usize {
        column.saturating_sub(self.first_column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_lines_have_bounded_utf8_windows_and_retain_absolute_columns() {
        let text = "a🦀é\u{301}".repeat(crate::documenttransfer::DOCUMENT_BYTES / 9);
        let total = text.chars().count();
        for cursor in [0, 255, 4096, total - 1, total, usize::MAX] {
            let viewport = Viewport::new(&text, cursor);
            let fragment = &text[viewport.bytes.clone()];
            assert!(fragment.chars().count() <= MAX_COLUMNS);
            assert!(fragment.len() <= MAX_COLUMNS * 4);
            assert_eq!(
                viewport.first_column,
                text[..viewport.bytes.start].chars().count()
            );
            let actual = cursor.min(total);
            assert!(viewport.first_column <= actual);
            assert!(viewport.local(actual) <= LEADING_CONTEXT * 2);
        }
        assert_eq!(Viewport::new("", usize::MAX).bytes, 0..0);
        let short = Viewport::new("a🦀é\u{301}", 4);
        assert_eq!(short.bytes, 0..9);
        assert_eq!(short.first_column, 0);
    }

    #[test]
    fn ordinary_graphemes_are_not_split_and_pathological_clusters_stay_bounded() {
        use unicode_segmentation::UnicodeSegmentation;
        for cluster in ["e\u{301}", "👩🏽‍💻", "🇨🇭"] {
            let text = cluster.repeat(300);
            let mut boundaries = text
                .grapheme_indices(true)
                .map(|(byte, _)| byte)
                .collect::<Vec<_>>();
            boundaries.push(text.len());
            for cursor in [17, 255, text.chars().count() - 1] {
                let viewport = Viewport::new(&text, cursor);
                assert!(boundaries.contains(&viewport.bytes.start));
                assert!(boundaries.contains(&viewport.bytes.end));
                assert!(text[viewport.bytes].chars().count() <= MAX_COLUMNS);
            }
        }
        let text = format!("e{}", "\u{301}".repeat(10_000));
        let viewport = Viewport::new(&text, 5000);
        assert!(text[viewport.bytes].chars().count() <= MAX_COLUMNS);
    }
}
