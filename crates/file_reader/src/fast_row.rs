//! DIAGNOSTIC ONLY (not for main): a one-font-run code row.
//!
//! GPUI's `StyledText` cuts a row into one font run per colour change, and
//! the macOS text system builds a font for each run, so a minified row with
//! dozens of colours costs many times a one-colour row in CoreText. When a
//! row is ASCII and its highlight runs set nothing but a colour, this
//! element lays the row out as ONE run and paints each glyph in the colour
//! of the run that covers it. The paint mirrors `paint_line` in
//! gpui-pre 0.3.7 (`src/text_system/line.rs`) for a left-aligned line with
//! no wrap boundaries and no decorations.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, HighlightStyle, Hsla, InspectorElementId,
    IntoElement, LayoutId, LineLayout, Pixels, SharedString, Style, TextRun, Window, point, px,
    size,
};

/// The colour runs of a row, when the row can take the one-run path: its
/// text is ASCII without a line break and no run sets anything but a colour.
/// `None` means the row falls back to `StyledText`.
pub(crate) fn colour_only(
    text: &str,
    runs: &[(Range<usize>, HighlightStyle)],
) -> Option<Vec<(Range<usize>, Hsla)>> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii() && b != b'\n') {
        return None;
    }
    let mut colours = Vec::with_capacity(runs.len());
    for (range, style) in runs {
        let HighlightStyle {
            color,
            font_weight,
            font_style,
            background_color,
            underline,
            strikethrough,
            fade_out,
        } = style;
        if font_weight.is_some()
            || font_style.is_some()
            || background_color.is_some()
            || underline.is_some()
            || strikethrough.is_some()
            || fade_out.is_some()
        {
            return None;
        }
        if let Some(color) = color {
            colours.push((range.clone(), *color));
        }
    }
    Some(colours)
}

/// The row's text, drawn glyph by glyph in the colours of `colours` (sorted,
/// not overlapping, byte ranges of the text); the base text colour where no
/// run covers a glyph.
pub(crate) struct FastRow {
    text: SharedString,
    colours: Vec<(Range<usize>, Hsla)>,
}

impl FastRow {
    pub(crate) fn new(text: SharedString, colours: Vec<(Range<usize>, Hsla)>) -> Self {
        Self { text, colours }
    }
}

/// What the layout pass hands to paint.
pub(crate) struct Laid {
    layout: Arc<LineLayout>,
    line_height: Pixels,
    base: Hsla,
}

impl IntoElement for FastRow {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FastRow {
    type RequestLayoutState = Laid;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // The same inputs as `TextLayout::layout` in gpui's `StyledText`.
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = window.pixel_snap(
            text_style
                .line_height
                .to_pixels(font_size.into(), window.rem_size()),
        );
        let run = TextRun {
            len: self.text.len(),
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = window
            .text_system()
            .layout_line(&self.text, font_size, &[run], None);
        let mut style = Style::default();
        style.size.width = layout.width.ceil().into();
        style.size.height = line_height.into();
        let layout_id = window.request_layout(style, [], cx);
        (
            layout_id,
            Laid {
                layout,
                line_height,
                base: text_style.color,
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _laid: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        laid: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let Laid {
            layout,
            line_height,
            base,
        } = laid;
        let origin = bounds.origin;
        // `line_paint_bounds` of a left-aligned line without wraps.
        let line_bounds = Bounds::new(origin, size(layout.width, *line_height));
        window.paint_layer(line_bounds, |window| {
            // `paint_line`: the baseline is the ascent below the top of the
            // line's box, whose free space is split above and below.
            let padding_top = (*line_height - layout.ascent - layout.descent) / 2.;
            let baseline = point(px(0.), padding_top + layout.ascent);
            let content_bounds = window.content_mask().bounds;
            for run in &layout.runs {
                let max_glyph_size = window
                    .text_system()
                    .bounding_box(run.font_id, layout.font_size)
                    .size;
                for glyph in &run.glyphs {
                    // The row has one line and is left-aligned: the glyph's
                    // x is the line's origin plus its position.
                    let glyph_origin = point(origin.x + glyph.position.x, origin.y);
                    if !Bounds::new(glyph_origin, max_glyph_size).intersects(&content_bounds) {
                        continue;
                    }
                    // The run is sorted and does not overlap: the covering
                    // run is the last one that starts at or before the glyph.
                    let at = self
                        .colours
                        .partition_point(|(range, _)| range.start <= glyph.index);
                    let color = at
                        .checked_sub(1)
                        .and_then(|at| self.colours.get(at))
                        .filter(|(range, _)| glyph.index < range.end)
                        .map_or(*base, |(_, color)| *color);
                    // ASCII only, so no glyph is an emoji.
                    let _ = window.paint_glyph(
                        glyph_origin + baseline + point(px(0.), glyph.position.y),
                        run.font_id,
                        glyph.id,
                        layout.font_size,
                        color,
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use gpui::{FontWeight, UnderlineStyle, hsla};

    use super::*;

    fn colour(color: Hsla) -> HighlightStyle {
        HighlightStyle {
            color: Some(color),
            ..HighlightStyle::default()
        }
    }

    #[test]
    fn fast_row_takes_colour_only_ascii_rows() {
        let red = hsla(0., 1., 0.5, 1.);
        let runs = vec![(0..3, colour(red)), (4..5, HighlightStyle::default())];
        assert_eq!(colour_only("let x", &runs), Some(vec![(0..3, red)]));
        assert_eq!(colour_only("let x", &[]), Some(vec![]));
    }

    #[test]
    fn fast_row_falls_back_for_anything_but_colour() {
        let red = hsla(0., 1., 0.5, 1.);
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..colour(red)
        };
        let underlined = HighlightStyle {
            underline: Some(UnderlineStyle::default()),
            ..colour(red)
        };
        let found = HighlightStyle {
            background_color: Some(red),
            ..colour(red)
        };
        for style in [bold, underlined, found] {
            assert_eq!(colour_only("let x", &[(0..3, style)]), None);
        }
        assert_eq!(colour_only("lét x", &[(0..1, colour(red))]), None);
        assert_eq!(colour_only("a\nb", &[]), None);
        assert_eq!(colour_only("", &[]), None);
    }
}
