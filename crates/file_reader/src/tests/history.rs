//! `history_*`: the back and forward lists (docs/design/file_reader.md「范围」).

use lectern_services::code_intel::TextOrigin;
use lectern_services::project::ProjectKey;
use lectern_services::reader::{Anchor, ReadTarget};

use crate::{HISTORY_MAX, History};

fn at(name: &str) -> ReadTarget {
    ReadTarget {
        project: ProjectKey::Folder {
            root: std::path::Path::new("/p").into(),
        },
        worktree: None,
        path: name.into(),
        origin: TextOrigin::Worktree,
        expect: None,
        at: Anchor::Start,
    }
}

fn names(history: &History) -> (Option<&str>, Option<&str>) {
    (
        history.peek_back().map(|t| &*t.path),
        history.peek_forward().map(|t| &*t.path),
    )
}

#[test]
fn history_is_empty_at_first() {
    let history = History::default();
    assert!(history.is_empty());
    assert!(!history.can_back() && !history.can_forward());
    assert_eq!(names(&history), (None, None));
}

#[test]
fn history_back_and_forward_walk_in_order() {
    let mut history = History::default();
    // a -> b -> c -> d, then back three times, forward once.
    history.visit(Some(at("a")));
    history.visit(Some(at("b")));
    history.visit(Some(at("c")));
    assert_eq!(history.len(), (3, 0));
    assert_eq!(names(&history), (Some("c"), None));
    history.went_back(Some(at("d")));
    assert_eq!(names(&history), (Some("b"), Some("d")));
    history.went_back(Some(at("c")));
    assert_eq!(names(&history), (Some("a"), Some("c")));
    history.went_back(Some(at("b")));
    assert_eq!(names(&history), (None, Some("b")));
    assert!(!history.can_back());
    history.went_forward(Some(at("a")));
    assert_eq!(names(&history), (Some("a"), Some("c")));
    assert_eq!(history.len(), (1, 2));
}

#[test]
fn history_a_visit_drops_what_was_ahead() {
    let mut history = History::default();
    history.visit(Some(at("a")));
    history.went_back(Some(at("b")));
    assert!(history.can_forward());
    history.visit(Some(at("a")));
    assert!(!history.can_forward());
    assert_eq!(history.len(), (1, 0));
}

#[test]
fn history_keeps_the_last_hundred_positions() {
    let mut history = History::default();
    for i in 0..HISTORY_MAX + 20 {
        history.visit(Some(at(&format!("f{i}"))));
    }
    assert_eq!(history.len().0, HISTORY_MAX);
    // The oldest were dropped, the nearest is the last visited.
    assert_eq!(history.peek_back().map(|t| &*t.path), Some("f119"));
    for _ in 0..HISTORY_MAX - 1 {
        history.went_back(None);
    }
    assert_eq!(history.peek_back().map(|t| &*t.path), Some("f20"));
}

#[test]
fn history_forward_is_capped_too() {
    let mut history = History::default();
    for i in 0..HISTORY_MAX + 5 {
        history.went_back(Some(at(&format!("f{i}"))));
    }
    assert_eq!(history.len().1, HISTORY_MAX);
    for i in 0..HISTORY_MAX + 5 {
        history.went_forward(Some(at(&format!("g{i}"))));
    }
    assert_eq!(history.len().0, HISTORY_MAX);
}

#[test]
fn history_without_a_position_to_leave_adds_nothing() {
    let mut history = History::default();
    history.visit(None);
    assert!(history.is_empty());
}
