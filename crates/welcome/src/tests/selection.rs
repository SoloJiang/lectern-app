//! The keyboard selection and the two small string functions of the view,
//! as pure functions (docs/design/welcome.md「Action 与快捷键」「状态与异常」).

use std::path::Path;

use crate::view::{Lens, Selection, display_path, node_key};

const BOTH: Lens = Lens {
    recent: 2,
    discovered: 2,
};

#[test]
fn next_walks_the_two_lists_as_one_and_stops_at_the_end() {
    let mut at = None;
    let mut seen = Vec::new();
    for _ in 0..6 {
        at = Selection::next(at, BOTH);
        seen.push(at);
    }
    assert_eq!(
        seen,
        [
            Some(Selection::Recent(0)),
            Some(Selection::Recent(1)),
            Some(Selection::Discovered(0)),
            Some(Selection::Discovered(1)),
            Some(Selection::Discovered(1)),
            Some(Selection::Discovered(1)),
        ]
    );
}

#[test]
fn previous_walks_back_and_stops_at_the_first_item() {
    assert_eq!(
        Selection::previous(None, BOTH),
        Some(Selection::Discovered(1))
    );
    assert_eq!(
        Selection::previous(Some(Selection::Discovered(0)), BOTH),
        Some(Selection::Recent(1))
    );
    assert_eq!(
        Selection::previous(Some(Selection::Recent(0)), BOTH),
        Some(Selection::Recent(0))
    );
}

#[test]
fn an_empty_list_is_skipped_and_two_empty_lists_select_nothing() {
    let only_discovered = Lens {
        recent: 0,
        discovered: 2,
    };
    assert_eq!(
        Selection::next(None, only_discovered),
        Some(Selection::Discovered(0))
    );
    let none = Lens {
        recent: 0,
        discovered: 0,
    };
    assert_eq!(Selection::next(None, none), None);
    assert_eq!(Selection::previous(None, none), None);
}

#[test]
fn clamping_keeps_the_position_in_the_same_list() {
    let lens = Lens {
        recent: 3,
        discovered: 1,
    };
    assert_eq!(
        Selection::Recent(1).clamped(lens),
        Some(Selection::Recent(1))
    );
    assert_eq!(
        Selection::Recent(7).clamped(lens),
        Some(Selection::Recent(2)),
        "out of range: the last item"
    );
    assert_eq!(
        Selection::Discovered(4).clamped(lens),
        Some(Selection::Discovered(0))
    );
    let empty = Lens {
        recent: 0,
        discovered: 1,
    };
    assert_eq!(
        Selection::Recent(0).clamped(empty),
        None,
        "the list is empty"
    );
}

#[test]
fn the_home_directory_is_written_as_a_tilde() {
    let home = Path::new("/Users/example");
    assert_eq!(
        display_path(Path::new("/Users/example/work/lectern"), Some(home)),
        "~/work/lectern"
    );
    assert_eq!(display_path(Path::new("/Users/example"), Some(home)), "~");
    assert_eq!(
        display_path(Path::new("/Users/examples/x"), Some(home)),
        "/Users/examples/x",
        "a sibling with the same prefix is not below home"
    );
    assert_eq!(display_path(Path::new("/srv/x"), None), "/srv/x");
}

#[test]
fn a_node_key_never_holds_what_the_node_format_forbids() {
    assert_eq!(node_key(Path::new("/a/b")), "/a/b");
    let key = node_key(Path::new("/odd]./name\nhere"));
    assert!(!key.contains("]."), "{key}");
    assert!(!key.contains('\n'), "{key}");
}
