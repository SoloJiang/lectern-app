//! The records of one frame as `ui-tree/1` (docs/design/ui_probe.md「记录到树」
//! and「`ui-tree/1`」), and the two ways to get a frame's records: `capture`
//! (the frame loop of a running application) and `snapshot` (the tests).

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use gpui::{App, Bounds, Pixels, Window};
use lectern_ui::semantic::{Record, Recorder, Role};
use serde::Serialize;

use crate::protocol::TREE_SCHEMA;

/// A length in logical pixels, to two decimals: `f32` widened to `f64`
/// prints as `0.30000001192…`.
fn num(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Size {
    pub w: f64,
    pub h: f64,
}

/// What the tree says about the window it was taken from.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WindowInfo {
    /// GPUI's `WindowId::as_u64`; the same value in `hello` and `ui_tree`.
    pub id: u64,
    pub title: String,
    /// The viewport, logical pixels.
    pub size: Size,
    pub scale: f64,
    /// The window is the active one.
    pub key: bool,
}

impl WindowInfo {
    pub fn of(window: &Window) -> WindowInfo {
        let size = window.viewport_size();
        WindowInfo {
            id: window.window_handle().window_id().as_u64(),
            title: window.window_title(),
            size: Size {
                w: num(size.width.into()),
                h: num(size.height.into()),
            },
            scale: num(window.scale_factor()),
            key: window.is_window_active(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LabelOut {
    pub key: Option<&'static str>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BoundsOut {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl BoundsOut {
    fn of(bounds: Bounds<Pixels>) -> BoundsOut {
        BoundsOut {
            x: num(bounds.origin.x.into()),
            y: num(bounds.origin.y.into()),
            w: num(bounds.size.width.into()),
            h: num(bounds.size.height.into()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StateOut {
    pub selected: bool,
    pub focused: bool,
    pub disabled: bool,
    pub expanded: Option<bool>,
    pub checked: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Node {
    /// The number the screenshot shows: the index in `Tree::nodes`.
    pub n: usize,
    pub id: String,
    pub role: &'static str,
    pub label: Option<LabelOut>,
    pub bounds: BoundsOut,
    pub visible: bool,
    pub state: StateOut,
    pub attrs: BTreeMap<&'static str, String>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Warning {
    /// `duplicate-id` or `bad-id`.
    pub kind: &'static str,
    pub id: String,
}

/// `ui-tree/1`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tree {
    pub schema: &'static str,
    pub window: WindowInfo,
    pub nodes: Vec<Node>,
    pub warnings: Vec<Warning>,
}

/// The serialised name of a role (kebab-case); an exhaustive `match`, so a
/// new role fails to compile here until it has one.
pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Window => "window",
        Role::Sidebar => "sidebar",
        Role::Toolbar => "toolbar",
        Role::Panel => "panel",
        Role::Group => "group",
        Role::List => "list",
        Role::ListItem => "list-item",
        Role::Tree => "tree",
        Role::TreeItem => "tree-item",
        Role::TabBar => "tab-bar",
        Role::Tab => "tab",
        Role::Button => "button",
        Role::Toggle => "toggle",
        Role::Text => "text",
        Role::TextField => "text-field",
        Role::SearchField => "search-field",
        Role::Link => "link",
        Role::Menu => "menu",
        Role::MenuItem => "menu-item",
        Role::Dialog => "dialog",
        Role::Diff => "diff",
        Role::DiffHunk => "diff-hunk",
        Role::DiffLine => "diff-line",
        Role::Comment => "comment",
        Role::Badge => "badge",
        Role::Progress => "progress",
    }
}

/// The ID grammar: `segment('.' segment)*`, a segment is `name` or
/// `name[key]`, a name is `[a-z0-9-]+`, a key is any UTF-8 without a newline
/// and ends at the first `]` that is followed by `.` or by the end.
pub fn check_id(id: &str) -> bool {
    let b = id.as_bytes();
    let mut i = 0;
    loop {
        let start = i;
        while i < b.len() && (b[i].is_ascii_lowercase() || b[i].is_ascii_digit() || b[i] == b'-') {
            i += 1;
        }
        if i == start {
            return false;
        }
        if i < b.len() && b[i] == b'[' {
            let key_start = i + 1;
            let mut j = key_start;
            loop {
                if j >= b.len() {
                    return false;
                }
                if b[j] == b']' && (j + 1 == b.len() || b[j + 1] == b'.') {
                    break;
                }
                j += 1;
            }
            // Both ends are ASCII, so they are character boundaries.
            if id[key_start..j].contains('\n') {
                return false;
            }
            i = j + 1;
        }
        if i == b.len() {
            return true;
        }
        if b[i] != b'.' {
            return false;
        }
        i += 1;
    }
}

/// The warnings for the IDs of one frame, in node order: `bad-id` for an ID
/// that breaks the grammar (`NodeId` checks it in debug builds only), and
/// `duplicate-id` once for each ID that more than one node has.
pub fn warnings<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<Warning> {
    let mut warnings = Vec::new();
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for id in ids {
        let times = seen.entry(id).or_insert(0);
        *times += 1;
        if *times == 1 && !check_id(id) {
            warnings.push(Warning {
                kind: "bad-id",
                id: id.to_owned(),
            });
        }
        if *times == 2 {
            warnings.push(Warning {
                kind: "duplicate-id",
                id: id.to_owned(),
            });
        }
    }
    warnings
}

impl Tree {
    /// Builds the tree from one frame's records. A pure function: `capture`
    /// and `snapshot` both end here, and the unit tests give it made-up
    /// records. The nodes are numbered in pre-order depth-first from the
    /// synthesised window node (`n = 0`); a record whose `parent` is not an
    /// earlier record hangs under the window.
    pub fn from_records(window: WindowInfo, records: Vec<Record>) -> Tree {
        let count = records.len();
        let mut kids: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut roots: Vec<usize> = Vec::new();
        for (i, record) in records.iter().enumerate() {
            match record.parent {
                Some(p) if p < i => kids[p].push(i),
                _ => roots.push(i),
            }
        }
        let mut order: Vec<usize> = Vec::with_capacity(count);
        let mut stack: Vec<usize> = roots.iter().rev().copied().collect();
        while let Some(i) = stack.pop() {
            order.push(i);
            stack.extend(kids[i].iter().rev());
        }
        let mut number = vec![0usize; count];
        for (k, &i) in order.iter().enumerate() {
            number[i] = k + 1;
        }

        let mut nodes = Vec::with_capacity(count + 1);
        nodes.push(Node {
            n: 0,
            id: format!("window[{}]", window.id),
            role: role_name(Role::Window),
            label: Some(LabelOut {
                key: None,
                text: window.title.clone(),
            }),
            bounds: BoundsOut {
                x: 0.0,
                y: 0.0,
                w: window.size.w,
                h: window.size.h,
            },
            visible: true,
            state: StateOut::default(),
            attrs: BTreeMap::new(),
            parent: None,
            children: roots.iter().map(|&i| number[i]).collect(),
        });

        let mut slots: Vec<Option<Record>> = records.into_iter().map(Some).collect();
        for &i in &order {
            let Some(record) = slots[i].take() else {
                continue;
            };
            let semantic = record.semantic;
            let id = semantic.id.as_str().to_owned();
            nodes.push(Node {
                n: number[i],
                id,
                role: role_name(semantic.role),
                label: semantic.label.map(|label| LabelOut {
                    key: label.key,
                    text: label.text.to_string(),
                }),
                bounds: BoundsOut::of(record.bounds),
                visible: record.visible,
                state: StateOut {
                    selected: semantic.state.selected,
                    focused: semantic.state.focused,
                    disabled: semantic.state.disabled,
                    expanded: semantic.state.expanded,
                    checked: semantic.state.checked,
                },
                attrs: semantic
                    .attrs
                    .into_iter()
                    .map(|(key, value)| (key, value.to_string()))
                    .collect(),
                parent: Some(match record.parent {
                    Some(p) if p < i => number[p],
                    _ => 0,
                }),
                children: kids[i].iter().map(|&k| number[k]).collect(),
            });
        }
        let warnings = warnings(nodes.iter().skip(1).map(|node| node.id.as_str()));
        Tree {
            schema: TREE_SCHEMA,
            window,
            nodes,
            warnings,
        }
    }

    /// Pretty-printed, for snapshots and reading; the wire uses
    /// `serde_json::to_value`.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// The result of `capture`.
pub struct Capture {
    pub tree: Tree,
    /// Physical pixels; present when `with_image` was true.
    pub image: Option<image::RgbaImage>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// `render_to_image` failed (no drawable, no renderer), or this is a
    /// release build, which does not call it.
    Render,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Recording,
    Finished,
    Cancelled,
}

/// Stops a `capture` that has not finished: its callbacks do nothing any
/// more, and if the recorder is armed for it, it is disarmed. Main thread.
#[derive(Clone)]
pub struct Cancel(Rc<Cell<Phase>>);

impl Cancel {
    pub fn cancel(&self) {
        match self.0.get() {
            Phase::Recording => {
                drop(Recorder::finish());
                self.0.set(Phase::Cancelled);
            }
            Phase::Waiting => self.0.set(Phase::Cancelled),
            Phase::Finished | Phase::Cancelled => {}
        }
    }
}

/// Draws `window` once more with the recorder armed and hands the frame's
/// tree to `done`. Two `Window::on_next_frame` callbacks: the first runs at
/// the start of the next frame request (before the draw) and arms the
/// recorder; the second runs at the start of the one after (after the draw),
/// disarms it, builds the `Tree` and, with `with_image`, renders the frame
/// just drawn to an image synchronously. Does not block. `done` is not
/// called if the capture is cancelled or the window closes first.
pub fn capture(
    window: &mut Window,
    with_image: bool,
    done: impl FnOnce(Result<Capture, CaptureError>) + 'static,
) -> Cancel {
    let phase = Rc::new(Cell::new(Phase::Waiting));
    let cancel = Cancel(phase.clone());
    window.on_next_frame(move |window, _| {
        if phase.get() != Phase::Waiting {
            return;
        }
        // Bypass the view cache, so every wrapper is prepainted.
        window.refresh();
        Recorder::begin(window.window_handle().window_id());
        phase.set(Phase::Recording);
        window.on_next_frame(move |window, _| {
            if phase.get() != Phase::Recording {
                return;
            }
            let records = Recorder::finish();
            phase.set(Phase::Finished);
            let tree = Tree::from_records(WindowInfo::of(window), records);
            let image = if !with_image {
                Ok(None)
            } else if cfg!(debug_assertions) {
                // `framebufferOnly` is off in debug builds only; reading a
                // release build's drawable back is not allowed.
                window
                    .render_to_image()
                    .map(Some)
                    .map_err(|_| CaptureError::Render)
            } else {
                Err(CaptureError::Render)
            };
            done(image.map(|image| Capture { tree, image }));
        });
    });
    cancel
}

/// For tests: the same recorder, without the frame loop. Arms the recorder,
/// draws the window once, disarms it and builds the tree. Main thread, blocks
/// until drawn (in a test, `VisualTestContext::update`).
pub fn snapshot(window: &mut Window, cx: &mut App) -> Tree {
    Recorder::begin(window.window_handle().window_id());
    window.draw(cx).clear(cx);
    let records = Recorder::finish();
    Tree::from_records(WindowInfo::of(window), records)
}
