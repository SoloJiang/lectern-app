//! `files_*`: the Files tree. The first part is the rows of a constructed
//! `DirTable` (no window); the second runs the panel in a test window over
//! the plugin in a kernel with the mocked services and a project snapshot
//! whose directory table is built here (synthetic paths, no file IO).

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, Context, Entity, InteractiveElement, IntoElement, Modifiers, ParentElement,
    Render, Styled, TestAppContext, VisualTestContext, Window, WindowHandle, div,
};
use lectern_i18n::{Locale, Strings};
use lectern_kernel::testing::TestKernel;
use lectern_services::code_intel::{CodeIntel, TextOrigin};
use lectern_services::project::{
    ActiveTree, ChangedParts, DirTable, FileKind, FileList, Head, ProjectEvent, ProjectKey,
    ProjectSnapshot, Projects, Scan, ScanError, Worktree, WorktreeKey,
};
use lectern_services::reader::{Anchor, ReadTarget, Reader};
use lectern_services::workspace::{Contributions, Milestone, SidebarSection, WorkspaceCommand};
use lectern_ui::semantic::{Record, Recorder};
use lectern_ui::{Appearance, Icon, init_fixed};

use super::rig::{FakeCodeIntel, FakeContributions, FakeProjects, Resources, Tree as Folder};
use crate::files::{Content, Node, Tree};
use crate::{FileReaderPlugin, FilesView, Session};

// ---- the rows -------------------------------------------------------------

fn list(paths: &[&str]) -> Arc<FileList> {
    let mut builder = FileList::builder();
    for path in paths {
        builder.push(path, FileKind::File);
    }
    Arc::new(builder.build())
}

fn table(paths: &[&str]) -> Arc<DirTable> {
    Arc::new(DirTable::new(list(paths)))
}

/// The rows as the panel shows them: two spaces per level, the name, `/`
/// after a directory.
fn show(tree: &Tree) -> Vec<String> {
    (0..tree.len())
        .map(|i| {
            let depth = tree.rows()[i].depth as usize;
            let dir = if matches!(tree.rows()[i].node, Node::Dir { .. }) {
                "/"
            } else {
                ""
            };
            format!("{}{}{dir}", "  ".repeat(depth), tree.name(i).unwrap())
        })
        .collect()
}

fn index_of(tree: &Tree, path: &str) -> usize {
    tree.position(path)
        .unwrap_or_else(|| panic!("{path} is not a visible row"))
}

#[test]
fn files_rows_are_directories_first_in_finder_order() {
    let tree = Tree::new(
        table(&[
            "README.md",
            "file10.txt",
            "file2.txt",
            "src/b.rs",
            "src/a.rs",
            "Zed/z.txt",
            "alpha/x",
        ]),
        HashSet::new(),
    );
    // Directories first and without regard to case, then the files with
    // `file2` before `file10`.
    assert_eq!(
        show(&tree),
        [
            "alpha/",
            "src/",
            "Zed/",
            "file2.txt",
            "file10.txt",
            "README.md"
        ]
    );
    assert!(tree.rows().iter().all(|line| line.depth == 0));
}

#[test]
fn files_expand_inserts_the_children_and_collapse_removes_them() {
    let mut tree = Tree::new(
        table(&[
            "src/lib.rs",
            "src/ui/mod.rs",
            "src/ui/view.rs",
            "tests/a.rs",
            "README.md",
        ]),
        HashSet::new(),
    );
    assert_eq!(show(&tree), ["src/", "tests/", "README.md"]);

    let splice = tree.expand(0).expect("a closed directory opens");
    assert_eq!((splice.at, splice.removed, splice.inserted), (1, 0, 2));
    assert_eq!(
        show(&tree),
        ["src/", "  ui/", "  lib.rs", "tests/", "README.md"]
    );
    assert_eq!(tree.open_at(0), Some(true));
    assert!(tree.expand(0).is_none(), "an open directory is not opened");

    // A directory inside: its rows go in below it, and the outer one's
    // collapse takes the whole run out.
    let ui = index_of(&tree, "src/ui");
    let splice = tree.expand(ui).unwrap();
    assert_eq!((splice.at, splice.inserted), (ui + 1, 2));
    assert_eq!(tree.len(), 7);
    let splice = tree.collapse(0).expect("an open directory closes");
    assert_eq!((splice.at, splice.removed, splice.inserted), (1, 4, 0));
    assert_eq!(show(&tree), ["src/", "tests/", "README.md"]);
    assert!(tree.collapse(0).is_none());
    assert!(tree.expand(2).is_none(), "a file does not open");

    // The inner directory kept its state: opened again, `ui` is open.
    tree.expand(0);
    assert_eq!(
        show(&tree),
        [
            "src/",
            "  ui/",
            "    mod.rs",
            "    view.rs",
            "  lib.rs",
            "tests/",
            "README.md"
        ]
    );
}

#[test]
fn files_a_large_directory_opens_in_one_insertion() {
    let names: Vec<String> = (0..2_000).map(|n| format!("big/f{n:04}.txt")).collect();
    let paths: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut tree = Tree::new(table(&paths), HashSet::new());
    assert_eq!(tree.len(), 1);
    let splice = tree.expand(0).unwrap();
    assert_eq!((splice.at, splice.removed, splice.inserted), (1, 0, 2_000));
    assert_eq!(tree.len(), 2_001);
    assert_eq!(tree.name(2_000), Some("f1999.txt"));
    let splice = tree.collapse(0).unwrap();
    assert_eq!((splice.removed, splice.inserted), (2_000, 0));
}

#[test]
fn files_single_child_chains_are_one_row() {
    let mut tree = Tree::new(
        table(&[
            "docs/a.md",
            "src/main/java/com/example/A.java",
            "src/main/java/com/example/B.java",
            "web/src/x.ts",
            "web/y.ts",
        ]),
        HashSet::new(),
    );
    // `src` has one subdirectory and no file, down to `example`; `web` has a
    // file, so it stays a row of its own.
    assert_eq!(show(&tree), ["docs/", "src/main/java/com/example/", "web/"]);
    let chain = index_of(&tree, "src/main/java/com/example");
    assert!(tree.folded(chain));
    assert!(!tree.folded(index_of(&tree, "docs")));
    assert_eq!(tree.name(chain), Some("src/main/java/com/example"));

    tree.expand(chain);
    assert_eq!(
        show(&tree)[1..4],
        ["src/main/java/com/example/", "  A.java", "  B.java"]
    );
    // Every directory of the chain is in the expanded set, so a table in
    // which the chain breaks keeps the state of each part.
    for dir in [
        "src",
        "src/main",
        "src/main/java",
        "src/main/java/com",
        "src/main/java/com/example",
    ] {
        assert!(tree.expanded().contains(dir), "{dir}");
    }
    tree.collapse(chain);
    assert!(tree.expanded().is_empty());
}

#[test]
fn files_a_nested_row_shows_the_names_below_its_parent_only() {
    let mut tree = Tree::new(
        table(&["a/f.txt", "a/b/c/d/x.txt", "a/b/c/d/y.txt"]),
        HashSet::new(),
    );
    tree.expand(0);
    // `a` has a file, so it is not folded; its child chain is `b/c/d`.
    assert_eq!(show(&tree), ["a/", "  b/c/d/", "  f.txt"]);
}

#[test]
fn files_submodules_are_files_that_do_not_open() {
    let mut builder = FileList::builder();
    builder.push("vendor/lib", FileKind::Submodule);
    builder.push("vendor/readme", FileKind::File);
    let mut tree = Tree::new(
        Arc::new(DirTable::new(Arc::new(builder.build()))),
        HashSet::new(),
    );
    tree.expand(0);
    assert_eq!(show(&tree), ["vendor/", "  lib", "  readme"]);
    assert!(tree.expand(1).is_none());
    assert!(tree.open_at(1).is_none());
}

#[test]
fn files_reveal_opens_every_directory_above_the_file() {
    let mut tree = Tree::new(
        table(&[
            "a/f.txt",
            "a/b/c.txt",
            "a/b/d/e.txt",
            "a/b/d/f.txt",
            "z.txt",
        ]),
        HashSet::new(),
    );
    assert_eq!(show(&tree), ["a/", "z.txt"]);
    let at = tree.reveal("a/b/d/f.txt").expect("the file is listed");
    assert_eq!(tree.path(at), Some("a/b/d/f.txt"));
    assert_eq!(
        show(&tree),
        [
            "a/",
            "  b/",
            "    d/",
            "      e.txt",
            "      f.txt",
            "    c.txt",
            "  f.txt",
            "z.txt"
        ]
    );
    assert!(tree.reveal("a/missing.txt").is_none());
    assert!(tree.reveal("a/b").is_none(), "a directory is not a file");
    // Already open: the rows stay as they are.
    assert_eq!(tree.reveal("z.txt"), Some(7));
}

#[test]
fn files_a_new_table_keeps_the_expansion_by_path() {
    let mut tree = Tree::new(
        table(&["a/b/c/x.txt", "a/b/c/y.txt", "docs/g.md"]),
        HashSet::new(),
    );
    let chain = index_of(&tree, "a/b/c");
    tree.expand(chain);
    tree.expand(index_of(&tree, "docs"));
    assert_eq!(
        show(&tree),
        ["a/b/c/", "  x.txt", "  y.txt", "docs/", "  g.md"]
    );

    // `a` gets a file of its own and `docs` goes: the chain breaks into `a`
    // and `b/c`, each open as it was; `docs` is forgotten.
    let next = Tree::new(
        table(&["a/b/c/x.txt", "a/b/c/y.txt", "a/z.txt", "new/n.txt"]),
        tree.into_expanded(),
    );
    assert_eq!(
        show(&next),
        ["a/", "  b/c/", "    x.txt", "    y.txt", "  z.txt", "new/"]
    );
    assert!(!next.expanded().contains("docs"));
}

// ---- the panel in a window -----------------------------------------------

/// The sidebar's region, which the bindings name.
struct Region {
    child: Option<gpui::AnyView>,
}

impl Render for Region {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sidebar")
            .key_context("Sidebar")
            .size_full()
            .children(self.child.clone())
    }
}

struct Rig {
    res: Resources,
    kernel: TestKernel,
    session: Arc<Session>,
    contributions: Arc<FakeContributions>,
    projects: Arc<FakeProjects>,
    key: ProjectKey,
    worktree: WorktreeKey,
    folder: Folder,
    window: WindowHandle<Region>,
    view: Option<Entity<FilesView>>,
}

/// A git project (nothing is read from the repository) whose active
/// worktree has `paths`, and the directory table in the state `dirs` says.
fn project(
    key: &ProjectKey,
    worktree: &WorktreeKey,
    folder: &Folder,
    paths: &[&str],
    generation: u64,
    dirs: impl FnOnce(&Arc<FileList>) -> Scan<Arc<DirTable>>,
) -> ProjectSnapshot {
    let list = list(paths);
    let dirs = dirs(&list);
    ProjectSnapshot {
        key: key.clone(),
        generation,
        name: "repo".into(),
        worktrees: Arc::from(vec![Worktree {
            key: worktree.clone(),
            root: folder.root.clone(),
            is_main: true,
            head: Head::Unknown,
            locked: false,
            prunable: false,
        }]),
        active: Some(ActiveTree {
            root: folder.root.clone(),
            worktree: Some(worktree.clone()),
            opened_at: "".into(),
            files: Scan::Ready(list),
            packages: Scan::Pending,
            dirs,
        }),
    }
}

fn ready(list: &Arc<FileList>) -> Scan<Arc<DirTable>> {
    Scan::Ready(Arc::new(DirTable::new(list.clone())))
}

impl Rig {
    fn new(cx: &mut TestAppContext, paths: &[&str]) -> Rig {
        cx.executor().allow_parking();
        cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
        let res = Resources::new();
        let plugin = FileReaderPlugin::new(res.deps());
        let sessions = plugin.sessions();
        let projects = Arc::new(FakeProjects::default());
        let contributions = Arc::new(FakeContributions::default());
        let kernel = TestKernel::new(plugin)
            .mock::<dyn Projects>(projects.clone())
            .mock::<dyn CodeIntel>(Arc::new(FakeCodeIntel::default()))
            .mock::<dyn Contributions>(contributions.clone());
        kernel.activate().expect("the plugin activates");
        kernel.inner().expect("the kernel").barrier();
        let session = sessions.lock().unwrap().upgrade().expect("a session");
        let folder = Folder::new();
        let (key, worktree) = folder.git();
        projects.set(project(&key, &worktree, &folder, paths, 1, ready));
        cx.update(|cx| contributions.bind(cx));
        let window = cx.add_window(|_, _| Region { child: None });
        Rig {
            res,
            kernel,
            session,
            contributions,
            projects,
            key,
            worktree,
            folder,
            window,
            view: None,
        }
    }

    /// `workspace` builds the panel: the factory of `SidebarPanel("files")`.
    fn show(&mut self, cx: &mut TestAppContext) {
        let panel = self.contributions.panel();
        let view = self
            .window
            .update(cx, |region, window, cx| {
                let view = (panel.build)(&(), window, cx);
                region.child = Some(view.clone());
                cx.notify();
                view
            })
            .expect("the window is open");
        self.view = Some(view.downcast::<FilesView>().expect("a files view"));
        self.draw(cx);
    }

    /// `workspace` drops the panel's view when the sidebar is hidden.
    fn hide(&mut self, cx: &mut TestAppContext) {
        self.window
            .update(cx, |region, _, cx| {
                region.child = None;
                cx.notify();
            })
            .unwrap();
        self.view = None;
        self.draw(cx);
    }

    fn visual(&self, cx: &mut TestAppContext) -> VisualTestContext {
        VisualTestContext::from_window(AnyWindowHandle::from(self.window), cx)
    }

    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.kernel.inner().expect("the kernel").barrier();
            self.res.settle();
            cx.run_until_parked();
        }
    }

    /// The next frame's callbacks run (where the milestones are reported).
    fn frame(&self, cx: &mut TestAppContext) {
        let mut visual = self.visual(cx);
        visual.update(|window, cx| window.simulate_next_frame(cx));
        visual.run_until_parked();
    }

    fn draw(&self, cx: &mut TestAppContext) {
        let mut visual = self.visual(cx);
        for _ in 0..2 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
    }

    fn view(&self) -> &Entity<FilesView> {
        self.view.as_ref().expect("the panel is shown")
    }

    fn rows(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.view().read_with(cx, |view, _| match &view.content {
            Content::Tree(tree) => show(tree),
            _ => Vec::new(),
        })
    }

    fn selected(&self, cx: &mut TestAppContext) -> Option<String> {
        self.view()
            .read_with(cx, |view, _| match (&view.content, view.selected) {
                (Content::Tree(tree), Some(i)) => tree.path(i).map(str::to_owned),
                _ => None,
            })
    }

    fn press(&self, cx: &mut TestAppContext, keys: &str) {
        let mut visual = self.visual(cx);
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
        self.draw(cx);
    }

    fn record(&self, cx: &mut TestAppContext) -> Vec<Record> {
        self.draw(cx);
        let mut visual = self.visual(cx);
        let id = visual.update(|window, _| window.window_handle().window_id());
        Recorder::begin(id);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        Recorder::finish()
    }

    /// A new snapshot of the project, as `project` publishes it.
    fn publish(&self, cx: &mut TestAppContext, snapshot: ProjectSnapshot) {
        let generation = snapshot.generation;
        self.projects.set(snapshot);
        self.kernel
            .inner()
            .expect("the kernel")
            .emitter()
            .emit(ProjectEvent::Changed {
                project: self.key.clone(),
                generation,
                parts: ChangedParts {
                    dirs: true,
                    ..ChangedParts::default()
                },
            });
        self.settle(cx);
        self.draw(cx);
    }

    fn target(&self, path: &str) -> ReadTarget {
        ReadTarget {
            project: self.key.clone(),
            worktree: Some(self.worktree.clone()),
            path: path.into(),
            origin: TextOrigin::Worktree,
            expect: None,
            at: Anchor::Start,
        }
    }
}

const SAMPLE: [&str; 6] = [
    "README.md",
    "src/lib.rs",
    "src/ui/mod.rs",
    "src/ui/view.rs",
    "tests/a.rs",
    "tests/b.rs",
];

#[gpui::test]
fn files_the_panel_and_its_entry_are_registered(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, &SAMPLE);
    let kinds = rig.contributions.kinds();
    assert!(
        kinds.contains(&"sidebar-panel:files".to_owned()),
        "{kinds:?}"
    );
    let entries = rig.contributions.entries();
    assert_eq!(entries.len(), 1);
    let (id, section, order, item) = &entries[0];
    assert_eq!(id, "files");
    assert_eq!(*section, SidebarSection::Views);
    // After Worktrees (order 0).
    assert!(*order > 0);
    assert_eq!(item.icon, Some(Icon::Files));
    assert_eq!(item.label.name(), "file-reader-files-title");
    assert_eq!(
        item.on_select,
        WorkspaceCommand::ShowSidebarPanel(Some("files".into()))
    );
}

#[gpui::test]
fn files_the_panel_lists_the_top_level_rows(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    assert_eq!(rig.rows(cx), ["src/", "tests/", "README.md"]);
    assert_eq!(rig.selected(cx), None);
    let records = rig.record(cx);
    let tree = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "files.tree")
        .expect("the tree node");
    let attr = |name: &str| {
        tree.semantic
            .attrs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    };
    assert_eq!(attr("rows").as_deref(), Some("3"));
    assert_eq!(attr("expanded").as_deref(), Some("0"));
    // The rows are nodes keyed by path, with their facts.
    let src = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "files.tree.item[src].facts")
        .expect("the row of src");
    assert_eq!(src.semantic.state.expanded, Some(false));
    let readme = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "files.tree.item[README.md].facts")
        .expect("the row of README.md");
    assert_eq!(readme.semantic.state.expanded, None);
    assert!(
        records
            .iter()
            .any(|r| r.semantic.id.as_str() == "files.header.back")
    );
}

#[gpui::test]
fn files_only_the_rows_in_view_are_built(cx: &mut TestAppContext) {
    let names: Vec<String> = (0..5_000).map(|n| format!("big/f{n:05}.txt")).collect();
    let paths: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut rig = Rig::new(cx, &paths);
    rig.show(cx);
    assert_eq!(rig.rows(cx), ["big/"]);
    rig.press(cx, "down right");
    assert_eq!(rig.rows(cx).len(), 5_001);
    let built = rig.view().read_with(cx, |view, _| view.built.get());
    assert!(built > 5 && built < 80, "{built} rows built of 5001");
}

#[gpui::test]
fn files_milestones_are_the_first_rows_and_each_opened_directory(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    assert!(rig.contributions.milestones().is_empty());
    rig.show(cx);
    rig.frame(cx);
    assert_eq!(rig.contributions.milestones(), [Milestone::FilesShown]);
    // Moving the selection draws again and reports nothing.
    rig.press(cx, "down");
    rig.frame(cx);
    assert_eq!(rig.contributions.milestones(), [Milestone::FilesShown]);
    // → opens the directory: its rows are drawn, one milestone.
    rig.press(cx, "right");
    rig.frame(cx);
    assert_eq!(
        rig.contributions.milestones(),
        [Milestone::FilesShown, Milestone::FilesExpanded]
    );
    // Closing it reports nothing; ↵ opens it again, and that does.
    rig.press(cx, "left");
    rig.frame(cx);
    assert_eq!(rig.contributions.milestones().len(), 2);
    rig.press(cx, "enter");
    rig.frame(cx);
    assert_eq!(
        rig.contributions.milestones(),
        [
            Milestone::FilesShown,
            Milestone::FilesExpanded,
            Milestone::FilesExpanded
        ]
    );
}

#[gpui::test]
fn files_arrows_select_expand_and_collapse(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    rig.press(cx, "down");
    assert_eq!(rig.selected(cx).as_deref(), Some("src"));
    // → opens; → again goes to the first row inside.
    rig.press(cx, "right");
    assert_eq!(
        rig.rows(cx),
        ["src/", "  ui/", "  lib.rs", "tests/", "README.md"]
    );
    assert_eq!(rig.selected(cx).as_deref(), Some("src"));
    rig.press(cx, "right");
    assert_eq!(rig.selected(cx).as_deref(), Some("src/ui"));
    rig.press(cx, "right");
    assert_eq!(rig.rows(cx).len(), 7);
    // ← closes; ← again goes to the directory that holds it.
    rig.press(cx, "left");
    assert_eq!(rig.rows(cx).len(), 5);
    rig.press(cx, "left");
    assert_eq!(rig.selected(cx).as_deref(), Some("src"));
    // A file: ← goes up to its directory, → does nothing.
    rig.press(cx, "down down down down down");
    assert_eq!(rig.selected(cx).as_deref(), Some("README.md"));
    rig.press(cx, "down");
    assert_eq!(
        rig.selected(cx).as_deref(),
        Some("README.md"),
        "stops at the end"
    );
    rig.press(cx, "right");
    assert_eq!(rig.rows(cx).len(), 5);
    rig.press(cx, "up up up up up up up");
    assert_eq!(
        rig.selected(cx).as_deref(),
        Some("src"),
        "stops at the start"
    );
    // ↵ on a directory closes and opens it.
    rig.press(cx, "enter");
    assert_eq!(rig.rows(cx).len(), 3);
    rig.press(cx, "enter");
    assert_eq!(rig.rows(cx).len(), 5);
    // Nothing was asked of the reader.
    assert_eq!(rig.session.pending(), 0);
}

#[gpui::test]
fn files_enter_opens_the_file_in_the_reader(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    rig.press(cx, "down right right right down");
    assert_eq!(rig.selected(cx).as_deref(), Some("src/ui/mod.rs"));
    rig.press(cx, "enter");
    let request = rig.session.next_request().expect("a request was queued");
    assert_eq!(request.target, rig.target("src/ui/mod.rs"));
    assert_eq!(rig.session.next_request(), None);
    // The panel goes first, so that the reader's Esc is not taken by it.
    assert_eq!(
        rig.contributions.dispatched()[0],
        WorkspaceCommand::ShowSidebarPanel(None)
    );
}

#[gpui::test]
fn files_a_click_opens_a_file_and_toggles_a_directory(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    let click = |rig: &Rig, cx: &mut TestAppContext, id: &str| {
        let records = rig.record(cx);
        let bounds = records
            .iter()
            .find(|r| r.semantic.id.as_str() == id)
            .unwrap_or_else(|| panic!("{id} is not drawn"))
            .bounds;
        let mut visual = rig.visual(cx);
        visual.simulate_click(bounds.center(), Modifiers::none());
        visual.run_until_parked();
        rig.draw(cx);
    };
    click(&rig, cx, "files.tree.item[src]");
    assert_eq!(rig.rows(cx).len(), 5);
    assert_eq!(rig.selected(cx).as_deref(), Some("src"));
    click(&rig, cx, "files.tree.item[src/lib.rs]");
    let request = rig.session.next_request().expect("a request was queued");
    assert_eq!(request.target, rig.target("src/lib.rs"));
    click(&rig, cx, "files.tree.item[src]");
    assert_eq!(rig.rows(cx).len(), 3);
    assert_eq!(rig.session.next_request(), None);
}

#[gpui::test]
fn files_the_back_arrow_returns_to_the_project_sidebar(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    let records = rig.record(cx);
    let back = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "files.header.back")
        .expect("the back arrow");
    let mut visual = rig.visual(cx);
    visual.simulate_click(back.bounds.center(), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(
        rig.contributions.dispatched(),
        [WorkspaceCommand::ShowSidebarPanel(None)]
    );
    // Esc is not bound in the panel: it goes on to `workspace`.
    let keys = rig.contributions.keys();
    assert!(
        keys.iter()
            .all(|k| k.0 != "escape" || k.2.as_deref() != Some("Sidebar > files"))
    );
}

#[gpui::test]
fn files_the_panel_reveals_the_file_the_reader_showed_last(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    // The reader showed a file and was left (`close`): the document is
    // gone, the last target is not.
    rig.session.open(rig.target("src/ui/view.rs"));
    let request = rig.session.next_request().unwrap();
    rig.session.apply(request);
    rig.session.close();
    assert!(rig.session.snapshot().is_none());

    rig.show(cx);
    assert_eq!(
        rig.rows(cx),
        [
            "src/",
            "  ui/",
            "    mod.rs",
            "    view.rs",
            "  lib.rs",
            "tests/",
            "README.md"
        ]
    );
    assert_eq!(rig.selected(cx).as_deref(), Some("src/ui/view.rs"));

    // The action does it again after the tree was changed.
    rig.press(cx, "up up up up");
    assert_eq!(rig.selected(cx).as_deref(), Some("src"));
    rig.press(cx, "left");
    assert_eq!(rig.rows(cx).len(), 3);
    let mut visual = rig.visual(cx);
    visual.update(|window, cx| {
        let action = cx
            .build_action("file_reader::RevealCurrent", None)
            .expect("the action exists by name");
        window.dispatch_action(action, cx);
    });
    visual.run_until_parked();
    assert_eq!(rig.selected(cx).as_deref(), Some("src/ui/view.rs"));
    assert_eq!(rig.rows(cx).len(), 7);
}

#[gpui::test]
fn files_the_last_file_of_another_worktree_is_not_revealed(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    let mut target = rig.target("src/lib.rs");
    target.worktree = Some(WorktreeKey(Arc::from(rig.folder.root.join("other"))));
    rig.session.open(target);
    // The request cannot be placed (no such worktree): nothing was shown.
    let request = rig.session.next_request().unwrap();
    rig.session.apply(request);
    assert_eq!(rig.session.last_target(), None);
    rig.show(cx);
    assert_eq!(rig.rows(cx), ["src/", "tests/", "README.md"]);
    assert_eq!(rig.selected(cx), None);
}

#[gpui::test]
fn files_expansion_and_selection_outlive_the_view(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    rig.press(cx, "down right right right down");
    assert_eq!(rig.selected(cx).as_deref(), Some("src/ui/mod.rs"));
    // The reader hides the sidebar: the view is dropped.
    rig.hide(cx);
    rig.show(cx);
    assert_eq!(rig.rows(cx).len(), 7);
    assert_eq!(rig.selected(cx).as_deref(), Some("src/ui/mod.rs"));
}

#[gpui::test]
fn files_another_project_starts_afresh(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    rig.press(cx, "down right");
    rig.hide(cx);
    let other = ProjectKey::Folder {
        root: rig.folder.root.clone(),
    };
    rig.projects.remove(&rig.key);
    rig.projects.set(project(
        &other,
        &rig.worktree,
        &rig.folder,
        &SAMPLE,
        1,
        ready,
    ));
    rig.show(cx);
    assert_eq!(rig.rows(cx), ["src/", "tests/", "README.md"]);
}

#[gpui::test]
fn files_a_new_table_keeps_the_expansion_and_the_selection(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    rig.press(cx, "down right right right down");
    assert_eq!(rig.selected(cx).as_deref(), Some("src/ui/mod.rs"));
    let before = rig.view().read_with(cx, |view, _| match &view.content {
        Content::Tree(tree) => Arc::as_ptr(tree.table()),
        _ => panic!("a tree"),
    });

    // The same table again changes nothing.
    let mut same = (*rig.projects.snapshot(&rig.key).expect("a snapshot")).clone();
    same.generation = 2;
    rig.publish(cx, same);
    let same = rig.view().read_with(cx, |view, _| match &view.content {
        Content::Tree(tree) => Arc::as_ptr(tree.table()),
        _ => panic!("a tree"),
    });
    assert_eq!(before, same);

    // A new file list and its table: a file is added under `tests` and one
    // under `src`; `src` and `src/ui` are still open and the selection is
    // still on `mod.rs`.
    let mut paths = SAMPLE.to_vec();
    paths.push("tests/c.rs");
    paths.push("src/ui/more.rs");
    rig.publish(
        cx,
        project(&rig.key, &rig.worktree, &rig.folder, &paths, 3, ready),
    );
    assert_eq!(
        rig.rows(cx),
        [
            "src/",
            "  ui/",
            "    mod.rs",
            "    more.rs",
            "    view.rs",
            "  lib.rs",
            "tests/",
            "README.md"
        ]
    );
    assert_eq!(rig.selected(cx).as_deref(), Some("src/ui/mod.rs"));
}

#[gpui::test]
fn files_while_the_next_table_is_built_the_old_rows_stay(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    rig.press(cx, "down right");
    let rows = rig.rows(cx);
    let mut paths = SAMPLE.to_vec();
    paths.push("extra.txt");
    // `project` publishes the new list with its table pending.
    rig.publish(
        cx,
        project(&rig.key, &rig.worktree, &rig.folder, &paths, 2, |_| {
            Scan::Pending
        }),
    );
    assert_eq!(rig.rows(cx), rows);
    rig.publish(
        cx,
        project(&rig.key, &rig.worktree, &rig.folder, &paths, 3, ready),
    );
    // Finder order: `extra.txt` before `README.md`, whatever the case.
    let rows = rig.rows(cx);
    assert_eq!(rows[rows.len() - 2..], ["extra.txt", "README.md"]);
}

#[gpui::test]
fn files_rows_of_another_worktree_do_not_stay(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    rig.show(cx);
    rig.press(cx, "down right");
    assert_eq!(rig.rows(cx).len(), 5);
    // Another worktree becomes the active one; its table is still pending:
    // the old rows would open files of the wrong tree.
    let other = WorktreeKey(Arc::from(rig.folder.root.join("other")));
    rig.publish(
        cx,
        project(
            &rig.key,
            &other,
            &rig.folder,
            &["x/y.rs", "z.rs"],
            2,
            |_| Scan::Pending,
        ),
    );
    assert!(rig.rows(cx).is_empty());
    assert!(
        rig.view()
            .read_with(cx, |v, _| matches!(v.content, Content::Loading { .. }))
    );
    rig.press(cx, "enter");
    assert_eq!(rig.session.pending(), 0);
    // Its table arrives: the rows are the new list's, and the target names
    // the new worktree.
    rig.publish(
        cx,
        project(&rig.key, &other, &rig.folder, &["x/y.rs", "z.rs"], 3, ready),
    );
    assert_eq!(rig.rows(cx), ["x/", "z.rs"]);
    rig.press(cx, "down down down");
    rig.press(cx, "enter");
    let request = rig.session.next_request().expect("a request was queued");
    assert_eq!(request.target.worktree, Some(other));
    assert_eq!(&*request.target.path, "z.rs");
}

#[gpui::test]
fn files_reveal_scrolls_a_long_list_to_the_file(cx: &mut TestAppContext) {
    let names: Vec<String> = (0..5_000).map(|n| format!("big/f{n:05}.txt")).collect();
    let mut paths: Vec<&str> = names.iter().map(String::as_str).collect();
    paths.push("README.md");
    let mut rig = Rig::new(cx, &paths);
    rig.session.open(rig.target("big/f04000.txt"));
    let request = rig.session.next_request().unwrap();
    rig.session.apply(request);
    rig.session.close();
    rig.show(cx);
    assert_eq!(rig.rows(cx).len(), 5_002);
    assert_eq!(rig.selected(cx).as_deref(), Some("big/f04000.txt"));
    // The row is in view: drawn, and inside the list's bounds.
    let records = rig.record(cx);
    let row = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "files.tree.item[big/f04000.txt]")
        .expect("the row of the file is built");
    assert!(row.visible, "the revealed row is scrolled into view");
    let built = rig.view().read_with(cx, |view, _| view.built.get());
    assert!(built < 80, "{built} rows built");
}

#[gpui::test]
fn files_states_without_rows(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    // The table is not built yet: loading, nothing drawn for 100 ms.
    rig.projects.set(project(
        &rig.key,
        &rig.worktree,
        &rig.folder,
        &SAMPLE,
        2,
        |_| Scan::Pending,
    ));
    rig.show(cx);
    assert!(
        rig.view()
            .read_with(cx, |v, _| matches!(v.content, Content::Loading { .. }))
    );
    let records = rig.record(cx);
    let loading = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "files.tree.loading")
        .expect("the loading node");
    assert!(
        loading
            .semantic
            .attrs
            .contains(&("phase", "nothing".into()))
    );

    // The table arrives; `src` is opened.
    rig.publish(
        cx,
        project(&rig.key, &rig.worktree, &rig.folder, &SAMPLE, 3, ready),
    );
    assert_eq!(rig.rows(cx).len(), 3);
    rig.press(cx, "down right");
    assert_eq!(rig.rows(cx).len(), 5);

    // The index cannot be read.
    rig.publish(
        cx,
        project(&rig.key, &rig.worktree, &rig.folder, &SAMPLE, 4, |_| {
            Scan::Unavailable(ScanError::IndexUnreadable("broken".into()))
        }),
    );
    let records = rig.record(cx);
    let message = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "files.tree.unavailable")
        .expect("the unavailable line");
    assert_eq!(
        message.semantic.label.as_ref().map(|l| l.key),
        Some(Some("file-reader-files-unavailable"))
    );

    // No tracked file.
    rig.publish(
        cx,
        project(&rig.key, &rig.worktree, &rig.folder, &[], 5, ready),
    );
    let records = rig.record(cx);
    let message = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "files.tree.empty")
        .expect("the empty line");
    assert_eq!(
        message.semantic.label.as_ref().map(|l| l.key),
        Some(Some("file-reader-files-empty"))
    );

    // And the tree is back with the next list: the expansion and the
    // selection of before the states are kept.
    rig.publish(
        cx,
        project(&rig.key, &rig.worktree, &rig.folder, &SAMPLE, 6, ready),
    );
    assert_eq!(rig.rows(cx).len(), 5);
    assert_eq!(rig.selected(cx).as_deref(), Some("src"));
}

#[gpui::test]
fn files_a_folder_project_lists_its_files_too(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx, &SAMPLE);
    let key = rig.folder.folder();
    rig.projects.remove(&rig.key);
    let list = list(&SAMPLE);
    rig.projects.set(ProjectSnapshot {
        key: key.clone(),
        generation: 1,
        name: "folder".into(),
        worktrees: Arc::from(Vec::new()),
        active: Some(ActiveTree {
            root: rig.folder.root.clone(),
            worktree: None,
            opened_at: "".into(),
            files: Scan::Ready(list.clone()),
            packages: Scan::Pending,
            dirs: ready(&list),
        }),
    });
    rig.show(cx);
    assert_eq!(rig.rows(cx), ["src/", "tests/", "README.md"]);
    rig.press(cx, "down down down down");
    rig.press(cx, "enter");
    let request = rig.session.next_request().unwrap();
    // A folder project has no worktree.
    assert_eq!(request.target.project, key);
    assert_eq!(request.target.worktree, None);
    assert_eq!(&*request.target.path, "README.md");
}
