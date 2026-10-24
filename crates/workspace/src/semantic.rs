//! The semantic nodes of the regions (docs/design/workspace.md「界面」的
//! 「UI 探针的语义节点」) as data, until `ui::semantic` (ui design step 4)
//! provides the `semantic(spec, child)` wrapper element and the `Recorder`.
//!
//! The types carry the names and shapes of the ui design's `semantic`
//! module (`NodeId`, `Role`, `Label`, `State`, `Semantic`), so the swap is
//! a change of `use` lines plus wrapping the region elements. The root view
//! builds the tree with `Workspace::inspect`; the snapshot tests render it
//! with [`render`].

use std::fmt::Write as _;

/// A stable node ID: `segment('.' segment)*`, a segment being `name` or
/// `name[key]`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(view: &'static str, area: &'static str, key: Option<&str>) -> NodeId {
        let mut id = format!("{view}.{area}");
        if let Some(key) = key {
            let _ = write!(id, "[{key}]");
        }
        NodeId(id)
    }

    pub fn child(&self, name: &'static str) -> NodeId {
        NodeId(format!("{}.{name}", self.0))
    }

    pub fn item(&self, key: &str) -> NodeId {
        NodeId(format!("{}.item[{key}]", self.0))
    }

    /// A keyed child: `parent.keyed("screen", "review")` → `….screen[review]`
    /// (the workspace design's `workspace.content.screen[<id>]`). Not in the
    /// ui design's `NodeId`; with `ui::semantic` it becomes
    /// `NodeId::new(view, area, key)` on the region's own name.
    pub fn keyed(&self, name: &'static str, key: &str) -> NodeId {
        NodeId(format!("{}.{name}[{key}]", self.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The roles the probe knows, as the ui design lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// A node's text and where it came from: `key` is the Fluent message ID,
/// None for verbatim text.
#[derive(Clone, PartialEq, Debug)]
pub struct Label {
    pub key: Option<&'static str>,
    pub text: String,
}

/// The facts the probe reads; no styling.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct State {
    pub selected: bool,
    pub focused: bool,
    pub disabled: bool,
    pub expanded: Option<bool>,
    pub checked: Option<bool>,
}

/// One node's declaration.
#[derive(Clone, PartialEq, Debug)]
pub struct Semantic {
    pub id: NodeId,
    pub role: Role,
    pub label: Option<Label>,
    pub state: State,
    pub attrs: Vec<(&'static str, String)>,
}

impl Semantic {
    pub fn new(id: NodeId, role: Role) -> Semantic {
        Semantic {
            id,
            role,
            label: None,
            state: State::default(),
            attrs: Vec::new(),
        }
    }

    pub fn attr(mut self, name: &'static str, value: impl Into<String>) -> Semantic {
        self.attrs.push((name, value.into()));
        self
    }

    pub fn label(mut self, key: Option<&'static str>, text: impl Into<String>) -> Semantic {
        self.label = Some(Label {
            key,
            text: text.into(),
        });
        self
    }
}

/// A node of one frame's tree: its declaration and its parent's index in
/// the same `Vec`.
#[derive(Clone, Debug)]
pub struct Node {
    pub semantic: Semantic,
    pub parent: Option<usize>,
}

/// A tree under construction, in declaration order.
#[derive(Default)]
pub struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    /// Adds a node under `parent` and returns its index.
    pub fn push(&mut self, parent: Option<usize>, semantic: Semantic) -> usize {
        self.nodes.push(Node { semantic, parent });
        self.nodes.len() - 1
    }

    pub fn into_nodes(self) -> Vec<Node> {
        self.nodes
    }
}

/// The tree as indented text, one node per line: `id [Role] label state
/// attrs`, for the insta snapshots.
pub fn render(nodes: &[Node]) -> String {
    let mut out = String::new();
    for (index, node) in nodes.iter().enumerate() {
        let depth = depth(nodes, index);
        let s = &node.semantic;
        for _ in 0..depth {
            out.push_str("  ");
        }
        let _ = write!(out, "{} [{:?}]", s.id.as_str(), s.role);
        if let Some(label) = &s.label {
            match label.key {
                Some(key) => {
                    let _ = write!(out, " label={key}");
                }
                None => {
                    let _ = write!(out, " text={:?}", label.text);
                }
            }
        }
        if s.state.selected {
            out.push_str(" selected");
        }
        if s.state.disabled {
            out.push_str(" disabled");
        }
        if let Some(checked) = s.state.checked {
            let _ = write!(out, " checked={checked}");
        }
        if let Some(expanded) = s.state.expanded {
            let _ = write!(out, " expanded={expanded}");
        }
        for (name, value) in &s.attrs {
            let _ = write!(out, " {name}={value}");
        }
        out.push('\n');
    }
    out
}

fn depth(nodes: &[Node], mut index: usize) -> usize {
    let mut depth = 0;
    while let Some(parent) = nodes[index].parent {
        depth += 1;
        index = parent;
    }
    depth
}
