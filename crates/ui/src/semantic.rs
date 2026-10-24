//! The semantic nodes of the UI probe (docs/design/ui.md「探针」). Step 3
//! holds the data the controls declare: `NodeId`, `Role`, `Label`, `State`
//! and `Semantic`, and the seam `declare` the controls pass their node
//! through. Step 4 adds the recording: `declare` then returns the
//! `SemanticElement` that records bounds, visibility and parents into the
//! `Recorder`, plus `semantic(...)` and `semantic_interactive(...)` for the
//! views' own containers. Until then `declare` is the identity.

use gpui::{ElementId, IntoElement, SharedString};

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

/// The seam every control passes its rendered element through, with the
/// node it declares. Step 3: the identity (the node is dropped). Step 4:
/// returns the `SemanticElement` that records the node when the `Recorder`
/// is armed, so the controls need no change then; `declared_at` will be the
/// control's caller through `#[track_caller]` on the public constructors.
#[inline]
pub(crate) fn declare<E: IntoElement>(spec: Semantic, element: E) -> E {
    let _ = spec;
    element
}
