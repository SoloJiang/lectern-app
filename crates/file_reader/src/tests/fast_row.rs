//! `fast_row_*`: the one-font-run code row (`fast_row.rs`) in the reading
//! view. The test platform's text system is wrapped in one that records
//! every `layout_line` the platform is asked for, so the tests can count the
//! font runs of each laid-out row: the glyph shapes come from gpui's
//! `NoopTextSystem`, nothing opens a real window, and CoreText is not
//! involved. What the row looks like on screen is not tested here
//! (docs/design/file_reader.md「已知限制」).

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use gpui::{
    Bounds, DevicePixels, Font, FontId, FontMetrics, FontRun, GlyphId, LineLayout, NoopTextSystem,
    Pixels, PlatformTextSystem, RenderGlyphParams, Result, Size, TestAppContext, TestDispatcher,
    TextRenderingMode,
};
use lectern_services::reader::Anchor;
use lectern_syntax::{Row, testing};

use super::view::Rig;

/// One `layout_line` the platform was asked for: the text and the number of
/// font runs it was given.
type Call = (String, usize);

/// `NoopTextSystem` that records each `layout_line`.
struct Counting {
    inner: NoopTextSystem,
    calls: Mutex<Vec<Call>>,
}

impl Counting {
    fn new() -> Self {
        Self {
            inner: NoopTextSystem::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl PlatformTextSystem for Counting {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.inner.add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        self.inner.all_font_names()
    }

    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        self.inner.font_id(descriptor)
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.inner.font_metrics(font_id)
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        self.inner.typographic_bounds(font_id, glyph_id)
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.inner.advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.inner.glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.inner.glyph_raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.inner.rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.calls
            .lock()
            .unwrap()
            .push((text.to_owned(), runs.len()));
        self.inner.layout_line(text, font_size, runs)
    }

    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        self.inner.recommended_rendering_mode(font_id, font_size)
    }
}

/// What `#[gpui::test]` does for a `&mut TestAppContext`, with the counting
/// text system in the platform (the macro cannot choose the text system).
fn with_counting(test: fn(&mut TestAppContext, &Counting)) {
    gpui::run_test(
        1,
        &[],
        0,
        &mut |dispatcher: TestDispatcher, _seed| {
            let counting = Arc::new(Counting::new());
            let mut cx = TestAppContext::build_with_text_system(
                dispatcher.clone(),
                Some("fast_row"),
                counting.clone(),
            );
            let _entity_refcounts = cx.app.borrow().ref_counts_drop_handle();
            test(&mut cx, &counting);
            cx.run_until_parked();
            cx.update(|cx| {
                cx.background_executor().forbid_parking();
                cx.quit();
            });
            cx.run_until_parked();
            drop(cx);
            dispatcher.drain_tasks();
            drop(dispatcher);
        },
        None,
    );
}

/// The calls that laid out a row of the file: long text that is part of the
/// source (the gutter numbers, the toolbar are not; the find field shows the
/// short word that was typed in it, which is why the text must be long).
fn code_calls(calls: Vec<Call>, source: &str) -> Vec<Call> {
    calls
        .into_iter()
        .filter(|(text, _)| text.len() >= 32 && source.contains(text.as_str()))
        .collect()
}

fn minified(cx: &mut TestAppContext, counting: &Counting) -> (Rig, String) {
    let rig = Rig::new(cx);
    let source = String::from_utf8(testing::minified_js(16 << 10, 0)).unwrap();
    rig.write("app.min.js", &source);
    counting.take();
    rig.open(cx, "app.min.js", Anchor::Start);
    (rig, source)
}

#[test]
fn fast_row_a_minified_js_row_is_laid_out_with_one_font_run() {
    with_counting(|cx, counting| {
        let (rig, source) = minified(cx, counting);
        // Control: the rows in view do have many colours, so a `StyledText`
        // row would have been cut into several font runs.
        let kinds = rig.read(cx, |view| {
            let snapshot = view.snapshot.as_ref().expect("a document");
            let rows = snapshot.rows(view.top, 1, view.cols());
            let Some(Row::Text { tokens, .. }) = rows.first() else {
                panic!("the first row has text");
            };
            tokens
                .iter()
                .map(|token| format!("{:?}", token.kind()))
                .collect::<BTreeSet<_>>()
        });
        assert!(kinds.len() >= 3, "{kinds:?}");
        let calls = code_calls(counting.take(), &source);
        assert!(calls.len() >= 2, "rows were laid out: {calls:?}");
        for (text, font_runs) in &calls {
            assert_eq!(*font_runs, 1, "{text}");
        }
    });
}

#[test]
fn fast_row_a_row_with_a_find_match_goes_through_styled_text() {
    with_counting(|cx, counting| {
        let (rig, source) = minified(cx, counting);
        counting.take();
        let mut visual = rig.visual(cx);
        visual.simulate_keystrokes("cmd-f");
        rig.draw(cx);
        // `function` is in the first display row: the match tints it, so
        // the row has a background and `StyledText` lays it out.
        rig.type_in_find(cx, "function");
        rig.settle(cx);
        rig.draw(cx);
        assert!(rig.read(cx, |view| {
            view.results().is_some_and(|r| !r.ranges.is_empty())
        }));
        let calls = code_calls(counting.take(), &source);
        let with_match: Vec<&Call> = calls
            .iter()
            .filter(|(text, _)| text.contains("function"))
            .collect();
        assert!(!with_match.is_empty(), "{calls:?}");
        for (text, font_runs) in with_match {
            assert!(*font_runs > 1, "{font_runs} font runs for {text}");
        }
        // The rows without a match stay on the one-run path (the fixture is
        // fixed: no match is cut by a display row's end).
        for (text, font_runs) in calls.iter().filter(|(t, _)| !t.contains("function")) {
            assert_eq!(*font_runs, 1, "{text}");
        }
    });
}
