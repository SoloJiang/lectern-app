//! The reading view's drawing (docs/design/file_reader.md「界面」「状态与
//! 异常」): the code area, a fixed-row-height list that lays out only the
//! display rows in view, the find bar and the messages of the states that
//! have no content. Colours and fonts come from `ui` (ADR 0015); the state
//! it reads is `view.rs`'s.

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, MouseMoveEvent, ParentElement, Render,
    ScrollWheelEvent, StatefulInteractiveElement, Styled, StyledText, Window, canvas, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Enter, Escape};
use lectern_i18n::{Args, Strings, keys};
use lectern_syntax::{DocSnapshot, DocState, LineNo, Row};
use lectern_ui::semantic::{Role, Semantic, State, semantic};
use lectern_ui::{
    Expander, LoadingPhase, MonoMetrics, Palette, Skeleton, Text, TextArea, TextRole, TextRoleExt,
    colors, loading_phase, mono_metrics,
};

use crate::actions::{
    self, Back, Close, DiscardMine, Edit, Find, FindNext, FindPrevious, Forward, KeepEditing,
    Merge, Save, SaveAnyway,
};
use crate::fast_row::{FastRow, colour_only};
use crate::nodes;
use crate::text::{expand, runs};
use crate::view::ReadingView;

impl Render for ReadingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *colors(window, cx);
        let mono = mono_metrics(window, cx);
        let strings = lectern_ui::strings(cx);
        self.sync_viewport(&mono);
        self.flush_edit(&palette);
        self.drawn_ready(window);
        let now = Instant::now();
        let height = self.find.height.tick(now, window);
        let opacity = self.find.opacity.tick(now, window);

        let body = self.render_body(&palette, &mono, &strings, window, cx);
        let mut column = div().size_full().flex().flex_col();
        if height > 0.0 {
            column = column.child(self.render_find(height, opacity, &strings, window, cx));
        }
        let column = column.child(body);
        let document = self.snapshot.as_ref().map(|snapshot| {
            let mut spec = Semantic::new(
                nodes::keyed("document", &nodes::document_key(&snapshot.target)),
                Role::Group,
            )
            .attr("state", state_name(&snapshot.state))
            .attr("find", if self.find.open { "open" } else { "closed" });
            if let Some(tier) = snapshot.tier {
                spec = spec.attr("tier", format!("{tier:?}"));
            }
            spec = spec.attr(
                "highlight",
                match snapshot.highlight {
                    lectern_syntax::Highlighting::Pending => "pending",
                    lectern_syntax::Highlighting::On => "on",
                    lectern_syntax::Highlighting::Plain => "plain",
                },
            );
            spec = spec.attr(
                "lines",
                match snapshot.lines {
                    lectern_syntax::LineCount::Exact(n) => format!("{n}"),
                    lectern_syntax::LineCount::Estimated(n) => format!("~{n}"),
                },
            );
            match &self.edit {
                Some(edit) => spec
                    .attr("mode", "editing")
                    .attr("dirty", edit.dirty().to_string()),
                None => spec.attr("mode", "readonly"),
            }
        });
        let column: AnyElement = match document {
            Some(spec) => semantic(spec, column).into_any_element(),
            None => column.into_any_element(),
        };
        // While editing, the context says so: the history's keys are the
        // editor's Outdent and Indent then.
        let context = if self.editing() {
            "reader editing"
        } else {
            actions::READER
        };
        div()
            .id("file-reader")
            .key_context(context)
            .track_focus(&self.focus)
            .size_full()
            .bg(palette.control_background)
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.find_next(cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.find_previous(cx)))
            .on_action(cx.listener(|this, _: &Back, window, cx| this.go_back(window, cx)))
            .on_action(cx.listener(|this, _: &Forward, window, cx| this.go_forward(window, cx)))
            .on_action(cx.listener(|this, _: &Close, window, cx| {
                if this.editing() {
                    // The editor did not take it either: what is left is
                    // leaving edit mode. Never `workspace`'s Esc.
                    this.leave_edit(window, cx);
                    return;
                }
                // No bar to close: the key goes on to `workspace`, which
                // goes back to the view that opened the reader.
                if !this.close_find(window, cx) {
                    cx.propagate();
                }
            }))
            // The editor's own Esc (it closes its popups and clears extra
            // cursors) goes on to the parents when it has nothing to close;
            // that is "leave edit mode", and it stops here so that
            // `workspace`'s Esc binding does not pop the screen under a
            // buffer.
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.editing() {
                    this.leave_edit(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Edit, window, cx| this.toggle_edit(window, cx)))
            // ⌘S is the Save button: Save Anyway after Keep Editing.
            .on_action(cx.listener(|this, _: &Save, _, cx| {
                let anyway = this.save_anyway();
                this.save(anyway, cx);
            }))
            .on_action(cx.listener(|this, _: &SaveAnyway, _, cx| {
                this.save(true, cx);
            }))
            .on_action(cx.listener(|this, _: &Merge, _, cx| this.merge(cx)))
            .on_action(
                cx.listener(|this, _: &DiscardMine, window, cx| this.discard_mine(window, cx)),
            )
            .on_action(cx.listener(|this, _: &KeepEditing, _, cx| this.keep_editing(cx)))
            .child(column)
    }
}

/// The probe's name of a state.
fn state_name(state: &DocState) -> &'static str {
    match state {
        DocState::Opening => "opening",
        DocState::Ready => "ready",
        DocState::Symlink { .. } => "symlink",
        DocState::Unsupported => "unsupported",
        DocState::Missing => "missing",
        DocState::Unreadable(_) => "unreadable",
    }
}

impl ReadingView {
    fn render_body(
        &mut self,
        palette: &Palette,
        mono: &MonoMetrics,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(snapshot) = self.snapshot.clone() else {
            return div().flex_1().into_any_element();
        };
        if self.editing() {
            return self.render_editing(palette, mono, strings, window, cx);
        }
        match &snapshot.state {
            DocState::Ready => self.render_code(&snapshot, palette, mono, window, cx),
            DocState::Opening => self.render_loading(strings, window, cx),
            DocState::Missing => {
                message(Text::key(strings, keys::file_reader::MISSING), window, cx)
            }
            DocState::Unreadable(_) => message(
                Text::key(strings, keys::file_reader::UNREADABLE),
                window,
                cx,
            ),
            DocState::Unsupported => message(
                Text::key(strings, keys::file_reader::UNSUPPORTED),
                window,
                cx,
            ),
            DocState::Symlink { target } => {
                let target = String::from_utf8_lossy(target);
                message(
                    Text::fmt(
                        strings,
                        keys::file_reader::SYMLINK,
                        &Args::new().str("target", &target),
                    ),
                    window,
                    cx,
                )
            }
        }
    }

    /// Nothing for 100 ms, skeleton blocks until a second, then the word
    /// (Esc leaves; interaction.md「状态与异常」).
    fn render_loading(
        &mut self,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let phase = loading_phase(self.opened_at.elapsed());
        let name = match phase {
            LoadingPhase::Nothing => "nothing",
            LoadingPhase::Skeleton => "skeleton",
            LoadingPhase::Progress => "progress",
        };
        let content: AnyElement = match phase {
            LoadingPhase::Nothing => {
                window.request_animation_frame();
                div().into_any_element()
            }
            LoadingPhase::Skeleton => {
                window.request_animation_frame();
                div()
                    .p(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(Skeleton::new(px(240.0), px(14.0)))
                    .child(Skeleton::new(px(420.0), px(14.0)))
                    .child(Skeleton::new(px(180.0), px(14.0)))
                    .child(Skeleton::new(px(360.0), px(14.0)))
                    .into_any_element()
            }
            LoadingPhase::Progress => {
                message(Text::key(strings, keys::file_reader::LOADING), window, cx)
            }
        };
        semantic(
            Semantic::new(nodes::plain("loading"), Role::Group).attr("phase", name),
            div().size_full().child(content),
        )
        .into_any_element()
    }

    /// The code area: the display rows in view, each with its line number.
    fn render_code(
        &mut self,
        snapshot: &Arc<DocSnapshot>,
        palette: &Palette,
        mono: &MonoMetrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cols = self.cols();
        // One row more than the viewport holds: the pixel offset shows a
        // part of the one below.
        let count = self.rows_visible() + 1;
        let rows = snapshot.rows(self.top, count, cols);
        self.drawn = rows
            .iter()
            .map(|row| match row {
                Row::Text { pos, .. } | Row::Pending { pos } => *pos,
            })
            .collect();
        self.built_rows = rows.len();
        let cell = mono.cell_width;
        let gutter = cell * self.gutter_cells() as f32;
        let current = self.current_range();
        let (scroll_x, offset) = (self.scroll_x, self.offset);
        let lines: Vec<AnyElement> = rows
            .iter()
            .map(|row| {
                code_row(
                    row,
                    current.as_ref(),
                    gutter,
                    cell,
                    scroll_x,
                    mono.line_height,
                    palette,
                )
            })
            .collect();
        let top = self.top;
        let measure = self.area.clone();
        let rows_shown = lines.len();
        let line_height = mono.line_height;
        let list = div()
            .id("file-reader-lines")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .text_role(TextRole::Code, window, cx)
            .font_family(mono.family)
            .line_height(line_height)
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                this.scroll_by(event.delta.pixel_delta(line_height), cx);
                cx.stop_propagation();
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.pointer_moved(event.position, cx);
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, _| {
                if !*hovered {
                    this.pointer_left();
                }
            }))
            .child(
                canvas(move |bounds, _, _| measure.set(bounds), |_, (), _, _| ())
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top(-offset)
                    .left_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .children(lines),
            );
        semantic(
            Semantic::new(nodes::plain("lines"), Role::List)
                .attr("top", format!("{}.{}", top.line_start, top.sub_row))
                .attr("rows", rows_shown.to_string()),
            list,
        )
        .into_any_element()
    }

    /// The current find match, as the byte range of the file.
    fn current_range(&self) -> Option<Range<u64>> {
        let results = self.results()?;
        let index = self.find.current?;
        results.ranges.get(index).cloned()
    }

    /// The bar: the field and the count, in the expander.
    fn render_find(
        &mut self,
        height: f32,
        opacity: f32,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let field_id = nodes::plain("find").child("field");
        let weak = cx.weak_entity();
        let field = TextArea::new(field_id, &self.find.field)
            .placeholder(Text::key(strings, keys::file_reader::FIND_PLACEHOLDER))
            .lines(1, 1)
            .on_cancel(move |window, cx| {
                let _ = weak.update(cx, |view, cx| view.close_find(window, cx));
            });
        let count = self.count_text(strings);
        let count_spec = {
            let mut spec = Semantic::new(nodes::plain("find").child("count"), Role::Text);
            if let Some(text) = &count {
                spec = spec.label(text);
            }
            if let Some(results) = self.results() {
                spec = spec
                    .attr("total", results.ranges.len().to_string())
                    .attr("capped", results.capped.to_string());
            }
            spec
        };
        let bar = div()
            .h(crate::view::FIND_BAR_HEIGHT)
            .px_3()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            // ↵ and ⇧↵ step through the matches; the box would insert a
            // line break. Taken before the box's own handler.
            .capture_action(cx.listener(|this, action: &Enter, _, cx| {
                if !action.secondary {
                    if action.shift {
                        this.find_previous(cx);
                    } else {
                        this.find_next(cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(div().flex_1().child(field))
            .child(semantic(
                count_spec,
                div()
                    .min_w(px(96.0))
                    .text_role(TextRole::Secondary, window, cx)
                    .when_some(count, |count, text| count.child(text.shared())),
            ));
        Expander::new(nodes::plain("find"), px(height), opacity, bar).into_any_element()
    }

    /// "3 of 41"; nothing while the field is empty.
    fn count_text(&self, strings: &Strings) -> Option<Text> {
        if self.find.query.is_empty() {
            return None;
        }
        let (total, capped) = self
            .results()
            .map_or((0, false), |results| (results.ranges.len(), results.capped));
        let current = self.find.current.map_or(0, |index| index + 1);
        Some(if capped {
            Text::fmt(
                strings,
                keys::file_reader::FIND_CAPPED,
                &Args::new().num("current", current as f64),
            )
        } else {
            Text::fmt(
                strings,
                keys::file_reader::FIND_COUNT,
                &Args::new()
                    .num("current", current as f64)
                    .num("total", total as f64),
            )
        })
    }
}

/// A line of secondary text in the middle of the area, for the states that
/// have no content.
fn message(text: Text, window: &mut Window, cx: &mut Context<ReadingView>) -> AnyElement {
    semantic(
        Semantic::new(nodes::plain("message"), Role::Text).label(&text),
        div()
            .flex_1()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_role(TextRole::Secondary, window, cx)
            .child(text.shared()),
    )
    .into_any_element()
}

/// One display row: the line number column and the text, shifted by the
/// horizontal scroll.
fn code_row(
    row: &Row<'_>,
    current: Option<&Range<u64>>,
    gutter: gpui::Pixels,
    cell: gpui::Pixels,
    scroll_x: gpui::Pixels,
    line_height: gpui::Pixels,
    palette: &Palette,
) -> AnyElement {
    let (key, number, estimated, text) = match row {
        Row::Text {
            pos,
            line,
            matches,
            found,
            text,
            tokens,
            ..
        } => {
            let shown = expand(text);
            let here = current
                .and_then(|current| matches.iter().position(|m| m == current))
                .and_then(|at| found.get(at));
            let styled = runs(&shown, tokens, found, here, palette);
            let (number, estimated) = match line {
                LineNo::Exact(n) => (n + 1, false),
                LineNo::Estimated(n) => (n + 1, true),
            };
            // A long line's later display rows have no number of their own.
            let number = (pos.sub_row == 0).then_some(number);
            // A colour-only ASCII row is laid out as one font run
            // (`fast_row`); any other style keeps `StyledText`.
            let element = if let Some(colours) = colour_only(&shown.text, &styled) {
                div()
                    .child(FastRow::new(shown.text.clone(), colours))
                    .into_any_element()
            } else if styled.is_empty() {
                div().child(shown.text.clone()).into_any_element()
            } else {
                div()
                    .child(StyledText::new(shown.text.clone()).with_highlights(styled))
                    .into_any_element()
            };
            (
                nodes::row_key(pos.line_start, pos.sub_row),
                number,
                estimated,
                element,
            )
        }
        Row::Pending { pos } => (
            nodes::row_key(pos.line_start, pos.sub_row),
            None,
            false,
            div().into_any_element(),
        ),
    };
    let numbers = div()
        .w(gutter)
        .flex_shrink_0()
        .flex()
        .justify_end()
        .pr(cell)
        .text_color(palette.secondary_label)
        .when_some(number, |gutter, n| gutter.child(n.to_string()));
    let code = div().flex_1().min_w_0().h_full().overflow_hidden().child(
        div()
            .flex_shrink_0()
            .ml(-scroll_x)
            .whitespace_nowrap()
            .child(text),
    );
    let mut spec = Semantic::new(nodes::keyed("lines", &key), Role::Text).state(State::default());
    if let Some(n) = number {
        spec = spec.attr("line", n.to_string());
    }
    spec = spec.attr("estimated", estimated.to_string());
    semantic(
        spec,
        div()
            .w_full()
            .h(line_height)
            .flex_shrink_0()
            .flex()
            .flex_row()
            .child(numbers)
            .child(code),
    )
    .into_any_element()
}
