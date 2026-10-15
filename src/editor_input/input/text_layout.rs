//! Measures the same text fragments drawn by the input renderer.

use gpui::{Bounds, Pixels, SharedString, Window, px};

#[derive(Debug, Clone, Copy, Default)]
pub(super) enum Segments {
    #[default]
    Plain,
    Caret {
        column: usize,
        width: f32,
    },
    Selection {
        start: usize,
        end: usize,
    },
}

/// Retains measured positions only, including for masked password inputs.
#[derive(Debug, Clone)]
pub(super) struct TextLayout {
    first_column: usize,
    bounds: Bounds<Pixels>,
    columns: Vec<f32>,
    stops: Vec<(usize, f32)>,
}

impl TextLayout {
    pub(super) fn measure(
        text: &str,
        segments: Segments,
        placeholder: bool,
        first_column: usize,
        bounds: Bounds<Pixels>,
        window: &Window,
    ) -> Self {
        let mut layout = Self {
            first_column,
            bounds,
            columns: vec![
                f32::NAN;
                if placeholder {
                    1
                } else {
                    text.chars().count() + 1
                }
            ],
            stops: Vec::new(),
        };
        if placeholder {
            layout.columns[0] = 0.0;
            layout.stops.push((0, 0.0));
            return layout;
        }
        let bytes = text
            .char_indices()
            .map(|(index, _)| index)
            .chain([text.len()])
            .collect::<Vec<_>>();
        let last = bytes.len() - 1;
        let parts = match segments {
            Segments::Plain => vec![(0, last, 0.0)],
            Segments::Caret { column, width } => {
                let column = column.min(last);
                vec![(0, column, width), (column, last, 0.0)]
            }
            Segments::Selection { start, end } => {
                let start = start.min(last);
                let end = end.min(last).max(start);
                vec![(0, start, 0.0), (start, end, 0.0), (end, last, 0.0)]
            }
        };
        let style = window.text_style();
        let size = style.font_size.to_pixels(window.rem_size());
        let mut offset = 0.0;
        for (start, end, gap) in parts {
            let fragment: SharedString = text[bytes[start]..bytes[end]].to_owned().into();
            let line = window.text_system().shape_line(
                fragment,
                size,
                &[style.to_run(bytes[end] - bytes[start])],
                None,
            );
            let mut glyphs = line
                .runs
                .iter()
                .flat_map(|run| &run.glyphs)
                .map(|glyph| (glyph.index, f32::from(glyph.position.x)))
                .collect::<Vec<_>>();
            glyphs.sort_by_key(|glyph| glyph.0);
            layout.append_fragment(
                &bytes,
                start..=end,
                offset,
                f32::from(line.width()),
                &glyphs,
            );
            offset += f32::from(line.width()) + gap;
        }
        layout
            .stops
            .sort_by(|left, right| left.1.total_cmp(&right.1).then(left.0.cmp(&right.0)));
        layout.stops.dedup_by(|right, left| right.1 == left.1);
        layout
    }

    fn append_fragment(
        &mut self,
        bytes: &[usize],
        columns: std::ops::RangeInclusive<usize>,
        offset: f32,
        width: f32,
        glyphs: &[(usize, f32)],
    ) {
        let start = *columns.start();
        let end = *columns.end();
        let mut glyph = 0;
        for column in columns {
            let byte = bytes[column] - bytes[start];
            while glyph < glyphs.len() && glyphs[glyph].0 < byte {
                glyph += 1;
            }
            if self.columns[column].is_nan() {
                self.columns[column] = offset + glyphs.get(glyph).map_or(width, |glyph| glyph.1);
            }
        }
        // Pointer hits land on glyph cluster starts, avoiding the middle of
        // a ligature or combining cluster. No repeated full-line glyph scans.
        for &(byte, x) in glyphs {
            if let Ok(column) = bytes.binary_search(&(bytes[start] + byte)) {
                self.stops.push((column, offset + x));
            }
        }
        self.stops.push((end, offset + width));
    }

    pub(super) fn column(&self, x: Pixels) -> usize {
        let x = f32::from(x - self.bounds.origin.x);
        let right = self.stops.partition_point(|stop| stop.1 < x);
        let column = match (
            right.checked_sub(1).and_then(|index| self.stops.get(index)),
            self.stops.get(right),
        ) {
            (Some(left), Some(right)) => {
                if (left.1 - x).abs() <= (right.1 - x).abs() {
                    left.0
                } else {
                    right.0
                }
            }
            (Some(stop), None) | (None, Some(stop)) => stop.0,
            (None, None) => 0,
        };
        self.first_column.saturating_add(column)
    }

    pub(super) fn range_bounds(&self, start: usize, end: usize) -> Bounds<Pixels> {
        let start = self.columns[start
            .saturating_sub(self.first_column)
            .min(self.columns.len() - 1)];
        let end = self.columns[end
            .saturating_sub(self.first_column)
            .min(self.columns.len() - 1)];
        Bounds {
            origin: gpui::point(
                self.bounds.origin.x + px(start.min(end)),
                self.bounds.origin.y,
            ),
            // Keep an insertion point visible to platform IME candidate windows.
            size: gpui::size(px((end - start).abs().max(1.5)), self.bounds.size.height),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowed_layout_maps_pointer_hits_and_ime_bounds_to_full_line_coordinates() {
        let layout = TextLayout {
            first_column: 1000,
            bounds: Bounds {
                origin: gpui::point(px(200.0), px(40.0)),
                size: gpui::size(px(90.0), px(18.0)),
            },
            columns: vec![0.0, 10.0, 16.0],
            stops: vec![(0, 0.0), (1, 10.0), (2, 16.0)],
        };
        let column = layout.column(px(210.0));
        assert_eq!(column, 1001);
        let text = format!("{}aé", "🦀".repeat(1000));
        assert_eq!(
            super::super::InputEntity::char_to_utf16(&text, column),
            2001
        );
        let range = layout.range_bounds(1000, 1001);
        assert_eq!(range.origin.x, px(200.0));
        assert_eq!(range.size.width, px(10.0));
        assert_eq!(layout.range_bounds(0, 0).origin.x, px(200.0));
        assert_eq!(
            layout.range_bounds(usize::MAX, usize::MAX).origin.x,
            px(216.0)
        );
        assert_eq!(layout.column(px(-1000.0)), 1000);
        assert_eq!(layout.column(px(10_000.0)), 1002);
    }

    #[test]
    fn measured_fragments_account_for_utf8_clusters_cursor_width_and_empty_lines() {
        let bounds = Bounds {
            origin: gpui::point(px(100.0), px(40.0)),
            size: gpui::size(px(180.0), px(18.0)),
        };
        let bytes = [0, 1, 5, 7]; // UTF-8 boundaries of a🦀é.
        let mut layout = TextLayout {
            first_column: 0,
            bounds,
            columns: vec![f32::NAN; 4],
            stops: Vec::new(),
        };
        layout.append_fragment(&bytes, 0..=1, 0.0, 5.0, &[(0, 0.0)]);
        layout.append_fragment(&bytes, 1..=3, 6.5, 27.0, &[(0, 0.0), (4, 20.0)]);
        assert_eq!(layout.columns, [0.0, 5.0, 26.5, 33.5]);
        assert_eq!(layout.column(px(126.5)), 2);
        assert_eq!(layout.range_bounds(1, 2).size.width, px(21.5));
        let mut empty = TextLayout {
            first_column: 0,
            bounds,
            columns: vec![f32::NAN],
            stops: Vec::new(),
        };
        empty.append_fragment(&[0], 0..=0, 0.0, 0.0, &[]);
        assert_eq!(empty.column(px(900.0)), 0);
        assert_eq!(empty.range_bounds(0, 0).origin, bounds.origin);
    }

    #[test]
    fn proportional_positions_preserve_cluster_boundaries_and_window_coordinates() {
        // A combining cluster occupies columns 0..2 at the same glyph, followed
        // by a wide emoji and a narrow letter. The cache holds no text.
        let layout = TextLayout {
            first_column: 0,
            bounds: Bounds {
                origin: gpui::point(px(200.0), px(80.0)),
                size: gpui::size(px(100.0), px(18.0)),
            },
            columns: vec![0.0, 9.0, 9.0, 31.0, 35.0],
            stops: vec![(0, 0.0), (2, 9.0), (3, 31.0), (4, 35.0)],
        };
        assert_eq!(layout.column(px(209.0)), 2);
        assert_eq!(layout.column(px(224.0)), 3);
        assert_eq!(layout.column(px(0.0)), 0);
        assert_eq!(layout.column(px(500.0)), 4);
        let selected = layout.range_bounds(2, 3);
        assert_eq!(selected.origin, gpui::point(px(209.0), px(80.0)));
        assert_eq!(selected.size.width, px(22.0));
        assert_eq!(layout.range_bounds(3, 3).size.width, px(1.5));
    }

    #[test]
    fn scaled_positions_and_cursor_gaps_do_not_change_unicode_or_utf16_indices() {
        for scale in [1.0, 1.5, 2.0] {
            let layout = TextLayout {
                first_column: 0,
                bounds: Bounds {
                    origin: gpui::point(px(120.0), px(40.0)),
                    size: gpui::size(px(160.0), px(18.0 * scale)),
                },
                // The input draws a 1.5-pixel cursor between the first letter
                // and an emoji; measuring both fragments accounts for it.
                columns: vec![0.0, 5.0 * scale, 25.0 * scale + 1.5, 32.0 * scale + 1.5],
                stops: vec![
                    (0, 0.0),
                    (1, 5.0 * scale),
                    (1, 5.0 * scale + 1.5),
                    (2, 25.0 * scale + 1.5),
                    (3, 32.0 * scale + 1.5),
                ],
            };
            assert_eq!(layout.column(px(120.0 + 5.0 * scale + 0.75)), 1);
            let column = layout.column(px(120.0 + 25.0 * scale + 1.5));
            assert_eq!(column, 2);
            assert_eq!(super::super::InputEntity::char_to_utf16("a🦀é", column), 3);
            assert_eq!(layout.range_bounds(1, 2).size.width, px(20.0 * scale + 1.5));
        }
    }
}
