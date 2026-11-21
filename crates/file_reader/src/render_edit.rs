//! The edit mode's drawing (docs/design/file_reader.md「界面」): the code
//! editor and the row under the toolbar that says the file changed on disk
//! (or is gone, or could not be saved). The state is `edit.rs`'s.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::Editor;
use lectern_i18n::{Args, Strings, keys};
use lectern_services::workspace::Milestone;
use lectern_ui::semantic::{Role, Semantic, semantic};
use lectern_ui::{
    Button, ButtonKind, ButtonSize, MonoMetrics, Palette, Text, TextRole, TextRoleExt,
};

use crate::nodes;
use crate::text::file_name;
use crate::view::ReadingView;

/// The height of the row under the toolbar.
const BANNER_HEIGHT: f32 = 36.0;

/// What the row says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Banner {
    /// The file changed on disk: Merge, Discard My Changes, Keep Editing.
    ChangedOnDisk,
    /// The file cannot be read as the buffer's text any more: only Discard My
    /// Changes is left (the buffer can be copied).
    Unavailable,
    /// The last save failed.
    SaveFailed,
}

impl ReadingView {
    /// The row to show now, if any.
    pub(crate) fn banner(&self) -> Option<Banner> {
        let edit = self.edit.as_ref()?;
        let state = edit.session.state();
        if state.unavailable {
            Some(Banner::Unavailable)
        } else if state.dirty_on_disk && !edit.keep_editing {
            Some(Banner::ChangedOnDisk)
        } else if edit.failed {
            Some(Banner::SaveFailed)
        } else {
            None
        }
    }

    /// The editor under the row (when there is one).
    pub(crate) fn render_editing(
        &mut self,
        palette: &Palette,
        mono: &MonoMetrics,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The milestone `app/enter_edit` ends in: the editor drawn, with the
        // focus in it. Once per edit mode.
        if let Some(edit) = self.edit.as_mut()
            && !edit.ready
        {
            edit.ready = true;
            let shared = self.shared.clone();
            window.on_next_frame(move |_, _| shared.report_milestone(Milestone::EditReady));
        }
        let Some(edit) = &self.edit else {
            return div().flex_1().into_any_element();
        };
        let editor = Editor::new(&edit.editor)
            .appearance(false)
            .bordered(false)
            .size_full()
            .font_family(mono.family)
            .text_size(TextRole::Code.size())
            .line_height(mono.line_height);
        let banner = self.banner();
        div()
            .size_full()
            .flex()
            .flex_col()
            .when_some(banner, |column, banner| {
                column.child(self.render_banner(banner, palette, strings, window, cx))
            })
            .child(div().flex_1().min_h_0().child(editor))
            .into_any_element()
    }

    fn render_banner(
        &self,
        banner: Banner,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use keys::file_reader as k;
        let name = self
            .snapshot
            .as_ref()
            .map(|snapshot| file_name(&snapshot.target.path).to_owned())
            .unwrap_or_default();
        let text = match banner {
            Banner::ChangedOnDisk => Text::key(strings, k::CHANGED_ON_DISK),
            Banner::Unavailable => Text::key(strings, k::UNAVAILABLE_WHILE_EDITING),
            Banner::SaveFailed => {
                Text::fmt(strings, k::SAVE_FAILED, &Args::new().str("name", &name))
            }
        };
        let weak = cx.weak_entity();
        let discard = {
            let weak = weak.clone();
            Button::new(
                nodes::plain("disk").child("discard"),
                Text::key(strings, k::DISCARD_MINE),
            )
            .size(ButtonSize::Small)
            .on_click(move |window, cx| {
                let _ = weak.update(cx, |view, cx| view.discard_mine(window, cx));
            })
        };
        let mut row = div()
            .h(px(BANNER_HEIGHT))
            .flex_shrink_0()
            .px_3()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .bg(palette.fill)
            .border_b_1()
            .border_color(palette.separator)
            .child(
                div()
                    .flex_1()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(text.shared()),
            );
        // (`text` is also the node's label, below.)
        let (id, role) = match banner {
            Banner::ChangedOnDisk => {
                let merge = {
                    let weak = weak.clone();
                    Button::new(
                        nodes::plain("disk").child("merge"),
                        Text::key(strings, k::MERGE),
                    )
                    .size(ButtonSize::Small)
                    .kind(ButtonKind::Primary)
                    .on_click(move |_, cx| {
                        let _ = weak.update(cx, |view, cx| view.merge(cx));
                    })
                };
                let keep = {
                    let weak = weak.clone();
                    Button::new(
                        nodes::plain("disk").child("keep"),
                        Text::key(strings, k::KEEP_EDITING),
                    )
                    .size(ButtonSize::Small)
                    .on_click(move |_, cx| {
                        let _ = weak.update(cx, |view, cx| view.keep_editing(cx));
                    })
                };
                row = row.child(merge).child(discard).child(keep);
                ("banner", Role::Group)
            }
            Banner::Unavailable => {
                row = row.child(discard);
                ("banner", Role::Group)
            }
            Banner::SaveFailed => ("failed", Role::Text),
        };
        let spec = Semantic::new(nodes::plain("disk").child(id), role)
            .label(&text)
            .attr("kind", banner.name());
        semantic(spec, row).into_any_element()
    }
}

impl Banner {
    /// The probe's name of the row.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Banner::ChangedOnDisk => "changed",
            Banner::Unavailable => "unavailable",
            Banner::SaveFailed => "save_failed",
        }
    }
}
