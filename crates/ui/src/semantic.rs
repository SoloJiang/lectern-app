//! The semantic nodes of the UI probe (docs/design/ui.md「探针」): the data a
//! node declares (`NodeId`, `Role`, `Label`, `State`, `Semantic`), the
//! wrapper elements that attach it to an element (`semantic`,
//! `semantic_interactive`, `SemanticElement`) and the `Recorder` that
//! collects, for one frame, the `Record` of every node with the bounds,
//! visibility and parent that only layout knows. `ui_probe` (tool layer)
//! reads the records; the view crates depend on this crate alone. The
//! controls declare their node through the crate-internal `declare`.

use std::cell::RefCell;
use std::panic::Location;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, StatefulInteractiveElement, Toggled, Window, WindowId,
    accesskit,
};

use crate::text::Text;

/// A stable ID: `segment('.' segment)*`, a segment is `name` or
/// `name[key]`, a name is `[a-z0-9-]+`, a key is any UTF-8 without a
/// newline and without `].`. `NodeId::new("review", "files",
/// Some("src/main.rs"))` is `review.files.item[src/main.rs]`. Business keys
/// are paths and session IDs, not indices; unique within a frame. Debug
/// builds check the format, release builds do not.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(SharedString);

impl NodeId {
    pub fn new(view: &'static str, area: &'static str, key: Option<&str>) -> NodeId {
        debug_assert!(is_name(view), "NodeId view {view:?} is not [a-z0-9-]+");
        debug_assert!(is_name(area), "NodeId area {area:?} is not [a-z0-9-]+");
        let id = NodeId(SharedString::from(format!("{view}.{area}")));
        match key {
            Some(key) => id.item(key),
            None => id,
        }
    }

    /// A child node: `parent.child("edit")` is `….edit`.
    pub fn child(&self, name: &'static str) -> NodeId {
        debug_assert!(is_name(name), "NodeId child {name:?} is not [a-z0-9-]+");
        NodeId(SharedString::from(format!("{}.{name}", self.0)))
    }

    /// An item node: `parent.item(key)` is `….item[key]`.
    pub fn item(&self, key: &str) -> NodeId {
        debug_assert!(is_key(key), "NodeId key holds a newline or `].`");
        NodeId(SharedString::from(format!("{}.item[{key}]", self.0)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The GPUI element ID of the control that carries this node.
    pub(crate) fn element_id(&self) -> ElementId {
        ElementId::Name(self.0.clone())
    }

    /// The element ID of one named part of that control (its animation,
    /// its slider, …).
    pub(crate) fn part(&self, name: &'static str) -> ElementId {
        ElementId::NamedChild(
            std::sync::Arc::new(self.element_id()),
            SharedString::from(name),
        )
    }

    /// `part` keyed by an index as well: a new index is a new element, so
    /// an animation on it starts again.
    pub(crate) fn part_indexed(&self, name: &'static str, index: usize) -> ElementId {
        ElementId::NamedChild(
            std::sync::Arc::new(ElementId::NamedInteger(self.0.clone(), index as u64)),
            SharedString::from(name),
        )
    }
}

impl From<&NodeId> for ElementId {
    fn from(id: &NodeId) -> ElementId {
        id.element_id()
    }
}

fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_key(key: &str) -> bool {
    !key.contains('\n') && !key.contains("].")
}

/// The role: the type name in the probe and the snapshots, as the
/// `ui_probe` design's table has it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Role {
    Window,
    Sidebar,
    Toolbar,
    Panel,
    Group,
    List,
    ListItem,
    Tree,
    TreeItem,
    TabBar,
    Tab,
    Button,
    Toggle,
    Text,
    TextField,
    SearchField,
    Link,
    Menu,
    MenuItem,
    Dialog,
    Diff,
    DiffHunk,
    DiffLine,
    Comment,
    Badge,
    Progress,
}

/// The text a node shows and where it came from: `key` is the Fluent
/// message ID (`Text::message_id`), None for verbatim text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Label {
    pub key: Option<&'static str>,
    pub text: SharedString,
}

impl Label {
    pub fn of(text: &Text) -> Label {
        Label {
            key: text.message_id(),
            text: text.shared(),
        }
    }
}

/// The facts the probe looks at; no style.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct State {
    pub selected: bool,
    pub focused: bool,
    pub disabled: bool,
    pub expanded: Option<bool>,
    pub checked: Option<bool>,
}

/// One node's declaration. `attrs` are the view's own key-values
/// (`("tier", "B")`, `("rows", "120")`), at most 8 per node.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Semantic {
    pub id: NodeId,
    pub role: Role,
    pub label: Option<Label>,
    pub state: State,
    pub attrs: Vec<(&'static str, SharedString)>,
}

impl Semantic {
    /// A node with no label, default state and no attributes.
    pub fn new(id: NodeId, role: Role) -> Semantic {
        Semantic {
            id,
            role,
            label: None,
            state: State::default(),
            attrs: Vec::new(),
        }
    }

    pub fn label(mut self, text: &Text) -> Semantic {
        self.label = Some(Label::of(text));
        self
    }

    pub fn state(mut self, state: State) -> Semantic {
        self.state = state;
        self
    }

    pub fn attr(mut self, key: &'static str, value: impl Into<SharedString>) -> Semantic {
        debug_assert!(self.attrs.len() < 8, "a node holds at most 8 attrs");
        self.attrs.push((key, value.into()));
        self
    }
}

/// One record of the recorder: the declaration, the node's bounds in this
/// frame (window coordinates), whether it is visible inside the content
/// mask, its parent's index in the same frame's records, and where it was
/// declared.
#[derive(Clone, Debug)]
pub struct Record {
    pub semantic: Semantic,
    pub bounds: Bounds<Pixels>,
    /// The bounds meet the content mask in force when the node was
    /// prepainted (a row scrolled out of its list is not visible).
    pub visible: bool,
    /// The index of the nearest declared ancestor in the same `Vec`; None
    /// for a node with no declared ancestor (the probe hangs it under the
    /// window root it synthesises).
    pub parent: Option<usize>,
    /// The call site of `semantic(...)` / `semantic_interactive(...)`. For
    /// a control's own node it is the control's `render` (GPUI calls that,
    /// not the view; docs/design/ui.md「与设计的差异」第 4 步).
    pub declared_at: &'static Location<'static>,
}

/// Whether the recorder is armed: the only thing a wrapper reads when it is
/// not (one relaxed load; the recorder is for the main thread, so nothing
/// else is ordered by it).
static ARMED: AtomicBool = AtomicBool::new(false);

/// The window being recorded, the records so far and the stack of declared
/// ancestors of the element being prepainted. Main thread only; empty (no
/// allocation) while the recorder is disarmed.
struct Frame {
    window: Option<WindowId>,
    records: Vec<Record>,
    stack: Vec<usize>,
}

thread_local! {
    static FRAME: RefCell<Frame> = const {
        RefCell::new(Frame { window: None, records: Vec::new(), stack: Vec::new() })
    };
}

/// The recorder: one per process, used on the main thread. `ui_probe` and
/// the tests `begin` before a frame and `finish` after it. The records come
/// in declaration (pre-order) order and `Record::parent` indexes the same
/// `Vec`. Known limitation: the armed flag is process-wide while the
/// records are per thread, so two threads must not record at once (the
/// tests run one per process under nextest).
pub struct Recorder;

impl Recorder {
    /// Arms the recorder for `window`: from now on the wrappers prepainted
    /// in that window push a record. Drops records of an earlier `begin`
    /// that `finish` did not take.
    pub fn begin(window: WindowId) {
        FRAME.with_borrow_mut(|frame| {
            frame.window = Some(window);
            frame.records.clear();
            frame.stack.clear();
        });
        ARMED.store(true, Ordering::Relaxed);
    }

    /// Disarms the recorder and takes the frame's records.
    pub fn finish() -> Vec<Record> {
        ARMED.store(false, Ordering::Relaxed);
        FRAME.with_borrow_mut(|frame| {
            frame.window = None;
            frame.stack.clear();
            std::mem::take(&mut frame.records)
        })
    }

    /// Whether `begin` has been called and `finish` has not. This is the
    /// whole cost of a wrapper while the probe is off.
    #[inline]
    pub fn is_armed() -> bool {
        ARMED.load(Ordering::Relaxed)
    }
}

/// Attaches `spec` to `child`: the wrapper records the node (bounds,
/// visibility, parent) when the recorder is armed and costs one relaxed
/// atomic read otherwise. It is transparent to layout, painting and GPUI's
/// element IDs. For a child that is a `StatefulInteractiveElement` use
/// `semantic_interactive`, which also writes the accessibility attributes.
#[track_caller]
pub fn semantic(spec: Semantic, child: impl IntoElement) -> SemanticElement {
    SemanticElement {
        spec,
        child: child.into_any_element(),
        declared_at: Location::caller(),
    }
}

/// `semantic`, and the same declaration written into GPUI's accessibility
/// attributes of `child` (`accessibility_id`, the mapped role, `aria_label`,
/// `aria_selected`, `aria_expanded`, `aria_toggled`): the ID the probe shows
/// is the `AXIdentifier` that screen readers and computer-use tools see.
/// The attributes are plain fields on `child`; GPUI reads them only while an
/// assistive technology is attached. `child` must be an element with an ID
/// (GPUI sets the attributes on `Stateful<Div>` and the like only).
#[track_caller]
pub fn semantic_interactive<E: StatefulInteractiveElement + IntoElement>(
    spec: Semantic,
    child: E,
) -> SemanticElement {
    let access = Access::of(&spec);
    let mut child = child.accessibility_id(access.id).role(access.role);
    if let Some(label) = access.label {
        child = child.aria_label(label);
    }
    if let Some(selected) = access.selected {
        child = child.aria_selected(selected);
    }
    if let Some(expanded) = access.expanded {
        child = child.aria_expanded(expanded);
    }
    if let Some(toggled) = access.toggled {
        child = child.aria_toggled(toggled);
    }
    SemanticElement {
        spec,
        child: child.into_any_element(),
        declared_at: Location::caller(),
    }
}

/// The accessibility attributes a declaration maps to. Pure, so the mapping
/// is testable without GPUI's accessibility tree (which `#[gpui::test]`
/// cannot reach).
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Access {
    pub(crate) id: SharedString,
    pub(crate) role: accesskit::Role,
    pub(crate) label: Option<SharedString>,
    pub(crate) selected: Option<bool>,
    pub(crate) expanded: Option<bool>,
    pub(crate) toggled: Option<Toggled>,
}

impl Access {
    pub(crate) fn of(spec: &Semantic) -> Access {
        Access {
            id: spec.id.0.clone(),
            role: accesskit_role(spec.role),
            label: spec.label.as_ref().map(|label| label.text.clone()),
            // Selection is a fact of the selectable roles even when false
            // ("not selected"); elsewhere it is only said when true.
            selected: (spec.state.selected || selectable(spec.role)).then_some(spec.state.selected),
            expanded: spec.state.expanded,
            toggled: spec.state.checked.map(|checked| {
                if checked {
                    Toggled::True
                } else {
                    Toggled::False
                }
            }),
        }
    }
}

fn selectable(role: Role) -> bool {
    matches!(
        role,
        Role::ListItem | Role::TreeItem | Role::Tab | Role::MenuItem
    )
}

/// GPUI's role for a probe role. No role maps to `GenericContainer`: GPUI
/// drops those from the accessibility tree (and asserts against setting one).
pub(crate) fn accesskit_role(role: Role) -> accesskit::Role {
    use accesskit::Role as A;
    match role {
        Role::Window => A::Window,
        Role::Sidebar => A::Complementary,
        Role::Toolbar => A::Toolbar,
        Role::Panel => A::Pane,
        Role::Group => A::Group,
        Role::List => A::List,
        Role::ListItem => A::ListItem,
        Role::Tree => A::Tree,
        Role::TreeItem => A::TreeItem,
        Role::TabBar => A::TabList,
        Role::Tab => A::Tab,
        Role::Button => A::Button,
        Role::Toggle => A::CheckBox,
        Role::Text => A::Label,
        Role::TextField => A::TextInput,
        Role::SearchField => A::SearchInput,
        Role::Link => A::Link,
        Role::Menu => A::Menu,
        Role::MenuItem => A::MenuItem,
        Role::Dialog => A::Dialog,
        Role::Diff | Role::DiffHunk => A::Group,
        Role::DiffLine => A::ListItem,
        Role::Comment => A::Comment,
        Role::Badge => A::Label,
        Role::Progress => A::ProgressIndicator,
    }
}

/// The wrapper element of `semantic(...)`. It has no GPUI element ID of its
/// own (the child's, if any, stays the child's), asks the child for the
/// layout and returns that layout, so the bounds it is given in prepaint are
/// the child's.
pub struct SemanticElement {
    spec: Semantic,
    child: AnyElement,
    declared_at: &'static Location<'static>,
}

impl SemanticElement {
    /// Pushes this node's record and makes it the parent of what the child
    /// prepaints. False (nothing pushed) when the recorder is armed for
    /// another window.
    #[cold]
    #[inline(never)]
    fn open(&self, bounds: Bounds<Pixels>, window: &Window) -> bool {
        let id = window.window_handle().window_id();
        FRAME.with_borrow_mut(|frame| {
            if frame.window != Some(id) {
                return false;
            }
            let visible = window.content_mask().bounds.intersects(&bounds);
            let index = frame.records.len();
            let parent = frame.stack.last().copied();
            frame.records.push(Record {
                semantic: self.spec.clone(),
                bounds,
                visible,
                parent,
                declared_at: self.declared_at,
            });
            frame.stack.push(index);
            true
        })
    }
}

impl Element for SemanticElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        Some(self.declared_at)
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let open = Recorder::is_armed() && self.open(bounds, window);
        self.child.prepaint(window, cx);
        if open {
            FRAME.with_borrow_mut(|frame| frame.stack.pop());
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

impl IntoElement for SemanticElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// The seam every control passes its rendered element through, with the
/// node it declares. Returns an `AnyElement` so that a control can choose
/// between a declared and a decorative element in one `match`;
/// `#[track_caller]` makes `declared_at` the control's `render`.
#[inline]
#[track_caller]
pub(crate) fn declare(spec: Semantic, element: impl IntoElement) -> AnyElement {
    semantic(spec, element).into_any_element()
}
