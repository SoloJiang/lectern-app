//! The Files tree's drawing (docs/design/file_reader.md「界面」的 Files):
//! the header (back arrow and title), and under it one `uniform_list` of
//! the visible rows, or the one line of a state without rows. A frame
//! builds only the rows in view, however many files the project has.

use std::ops::Range;
use std::time::Instant;

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, Styled, Window, div, px, svg, uniform_list,
};
use lectern_i18n::{Strings, keys};
use lectern_services::workspace::Milestone;
use lectern_ui::semantic::{Role, Semantic, State, semantic};
use lectern_ui::{
    Button, Disclosure, Icon, LoadingPhase, Palette, SidebarRow, Skeleton, Text, TextRole,
    TextRoleExt, colors, loading_phase, strings,
};

use super::tree::{Node, Tree};
use super::view::{Content, FilesView};
use crate::actions::{
    FILES, FilesCollapse, FilesExpand, FilesNext, FilesOpen, FilesPrevious, RevealCurrent,
};
use crate::nodes;

/// The inset of the rows from the panel's edge, as the project sidebar's
/// (its rows are `SidebarRow::compact`'s 28pt).
const SIDE: Pixels = px(10.0);
/// What each level adds to the indent of a row.
const INDENT: Pixels = px(12.0);
const ICON_SIZE: Pixels = px(16.0);
/// The gap inside a row's content, as `SidebarRow::compact`'s.
const GAP: Pixels = px(8.0);
/// The header's height: the title's row.
const HEADER_HEIGHT: Pixels = px(36.0);
const BLOCK_HEIGHT: Pixels = px(10.0);

impl Render for FilesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strings = strings(cx);
        let header = self.header(&strings, window, cx);
        let body = self.body(&strings, window, cx);
        self.report_drawn(window);
        div()
            .id("files")
            .key_context(FILES)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &FilesNext, _, cx| this.select_next(cx)))
            .on_action(cx.listener(|this, _: &FilesPrevious, _, cx| this.select_previous(cx)))
            .on_action(cx.listener(|this, _: &FilesExpand, _, cx| this.expand(cx)))
            .on_action(cx.listener(|this, _: &FilesCollapse, _, cx| this.collapse(cx)))
            .on_action(cx.listener(|this, _: &FilesOpen, _, cx| this.activate(cx)))
            .on_action(cx.listener(|this, _: &RevealCurrent, _, cx| this.reveal_current(cx)))
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl FilesView {
    /// The milestones of this frame: the tree's first rows (`app/files_open`),
    /// and a directory that was opened (`app/files_expand`). Each is reported
    /// on the next frame, as the reading view's `FileHighlighted` is.
    fn report_drawn(&mut self, window: &mut Window) {
        if !matches!(self.content, Content::Tree(_)) {
            return;
        }
        let milestone = if !self.shown {
            self.shown = true;
            self.expanded_pending = false;
            Milestone::FilesShown
        } else if self.expanded_pending {
            self.expanded_pending = false;
            Milestone::FilesExpanded
        } else {
            return;
        };
        let shared = self.shared.clone();
        window.on_next_frame(move |_, _| shared.report_milestone(milestone));
    }

    /// The back arrow and the title. The arrow is the way back to the
    /// project sidebar, as Esc is.
    fn header(&self, strings: &Strings, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let back = Button::icon_only(
            nodes::files_header().child("back"),
            Icon::Back,
            Text::key(strings, keys::workspace::BACK),
        )
        .on_click(move |_, cx| {
            let _ = weak.update(cx, |view, cx| view.close(cx));
        });
        let title = Text::key(strings, keys::file_reader::FILES_TITLE);
        semantic(
            Semantic::new(nodes::files_header(), Role::Group),
            div()
                .flex()
                .items_center()
                .gap(GAP)
                .h(HEADER_HEIGHT)
                .flex_shrink_0()
                .px(SIDE)
                .child(back)
                .child(semantic(
                    Semantic::new(nodes::files_header().child("title"), Role::Text).label(&title),
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_role(TextRole::Headline, window, cx)
                        .child(title.shared()),
                )),
        )
        .into_any_element()
    }

    fn body(
        &mut self,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &self.content {
            Content::Loading { since } => loading(*since, strings, window, cx),
            Content::Unavailable => message(
                "unavailable",
                Text::key(strings, keys::file_reader::FILES_UNAVAILABLE),
                window,
                cx,
            ),
            Content::Empty => message(
                "empty",
                Text::key(strings, keys::file_reader::FILES_EMPTY),
                window,
                cx,
            ),
            Content::Tree(tree) => {
                let rows = tree.len();
                let expanded = tree.expanded().len();
                let list = uniform_list(
                    "files-tree-list",
                    rows,
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        this.build_rows(range, window, cx)
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full();
                semantic(
                    Semantic::new(nodes::files_tree(), Role::Tree)
                        .attr("rows", rows.to_string())
                        .attr("expanded", expanded.to_string())
                        .attr("visible", self.built.get().to_string()),
                    list,
                )
                .into_any_element()
            }
        }
    }

    /// The rows of `range`.
    fn build_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.built.set(range.len());
        let palette = *colors(window, cx);
        let Content::Tree(tree) = &self.content else {
            return Vec::new();
        };
        let weak = cx.entity().downgrade();
        range
            .filter_map(|index| {
                let weak = weak.clone();
                row(
                    tree,
                    index,
                    self.selected == Some(index),
                    &palette,
                    move |cx| {
                        let _ = weak.update(cx, |view, cx| view.click(index, cx));
                    },
                    window,
                    cx,
                )
            })
            .collect()
    }
}

/// One row: the disclosure arrow (a directory's) or its room, the icon, the
/// name. A folded chain is one row with its names joined by slashes.
fn row(
    tree: &Tree,
    index: usize,
    selected: bool,
    palette: &Palette,
    on_click: impl Fn(&mut gpui::App) + 'static,
    window: &mut Window,
    cx: &mut gpui::App,
) -> Option<AnyElement> {
    let line = *tree.rows().get(index)?;
    let path = tree.path(index)?;
    let name = tree.name(index)?;
    let open = tree.open_at(index);
    let node = nodes::files_row(path);
    let label = Text::verbatim(name.to_owned());
    let facts = Semantic::new(node.child("facts"), Role::Group)
        .label(&label)
        .state(State {
            selected,
            expanded: open,
            ..State::default()
        })
        .attr(
            "kind",
            match line.node {
                Node::Dir { .. } => "dir",
                Node::File(_) => "file",
            },
        )
        .attr("chain", if tree.folded(index) { "yes" } else { "no" })
        .attr("depth", line.depth.to_string());
    let (glyph, colour) = match line.node {
        Node::Dir { .. } => (Icon::Folder, palette.accent),
        Node::File(_) => (Icon::Files, palette.secondary_label),
    };
    let arrow: AnyElement = match open {
        Some(open) => Disclosure::new(if open { 1.0 } else { 0.0 }).into_any_element(),
        // A file has no arrow; its room keeps the icons in a column.
        None => div().w(px(12.0)).flex_shrink_0().into_any_element(),
    };
    let content = div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(GAP)
        .pl(INDENT * f32::from(line.depth))
        .text_role(TextRole::Body, window, cx)
        .child(arrow)
        .child(
            svg()
                .path(glyph.path())
                .size(ICON_SIZE)
                .flex_shrink_0()
                .text_color(colour),
        )
        .child(div().flex_1().min_w_0().truncate().child(label.shared()));
    Some(
        div()
            .flex()
            .flex_col()
            .w_full()
            .flex_shrink_0()
            .px(SIDE)
            .child(
                SidebarRow::new(node)
                    .compact()
                    .selected(selected)
                    .on_click(move |_, cx| on_click(cx))
                    .child(semantic(facts, content)),
            )
            .into_any_element(),
    )
}

/// One line of text where the rows would be.
fn message(
    name: &'static str,
    text: Text,
    window: &mut Window,
    cx: &mut Context<FilesView>,
) -> AnyElement {
    semantic(
        Semantic::new(nodes::files_tree().child(name), Role::Text).label(&text),
        div()
            .w_full()
            .px(SIDE + px(8.0))
            .py(px(8.0))
            .text_role(TextRole::Secondary, window, cx)
            .child(text.shared()),
    )
    .into_any_element()
}

/// Nothing for 100 ms, skeleton blocks until a second, then the word
/// (interaction.md「状态与异常」).
fn loading(
    since: Instant,
    strings: &Strings,
    window: &mut Window,
    cx: &mut Context<FilesView>,
) -> AnyElement {
    let phase = loading_phase(since.elapsed());
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
                .px(SIDE + px(8.0))
                .py(px(8.0))
                .flex()
                .flex_col()
                .gap(px(14.0))
                .font_weight(FontWeight::NORMAL)
                .child(Skeleton::new(px(120.0), BLOCK_HEIGHT))
                .child(Skeleton::new(px(160.0), BLOCK_HEIGHT))
                .child(Skeleton::new(px(96.0), BLOCK_HEIGHT))
                .into_any_element()
        }
        LoadingPhase::Progress => message(
            "progress-text",
            Text::key(strings, keys::file_reader::LOADING),
            window,
            cx,
        ),
    };
    semantic(
        Semantic::new(nodes::files_tree().child("loading"), Role::Group).attr("phase", name),
        div().size_full().child(content),
    )
    .into_any_element()
}
