//! The keyboard selection and the two small string functions of the view,
//! as pure functions (docs/design/welcome.md「Action 与快捷键」「状态与异常」).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_i18n::{Locale, Strings};
use lectern_services::session::{Agent, Timestamp};

use super::support::{MINUTE, NOW, folder_key, git_key};
use crate::DiscoveredFolder;
use crate::recent::RecentEntry;
use crate::view::{
    Lens, Selection, discovered_detail, display_path, node_key, recent_detail, subtitle,
};

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

const HOME: &str = "/Users/example";

/// Fluent wraps a string argument in first-strong isolates; the text the
/// reader sees is without them.
fn plain(text: impl AsRef<str>) -> String {
    text.as_ref().replace(['\u{2068}', '\u{2069}'], "")
}

fn recent(path: &str, folder: bool, sessions: u32, running: u32) -> RecentEntry {
    RecentEntry {
        path: PathBuf::from(path),
        key: if folder {
            folder_key(path)
        } else {
            git_key(&format!("{path}/.git"))
        },
        name: Arc::from(path.rsplit('/').next().unwrap()),
        last_opened: NOW,
        missing: false,
        sessions,
        running,
    }
}

fn discovered(agents: &[Agent], sessions: u32, running: u32, age_minutes: i64) -> DiscoveredFolder {
    DiscoveredFolder {
        path: Arc::from(Path::new("/Users/example/work/fresh")),
        name: Arc::from("fresh"),
        key: git_key("/Users/example/work/fresh/.git"),
        agents: agents.into(),
        sessions,
        running,
        newest_activity: Timestamp(NOW - age_minutes * MINUTE),
    }
}

#[test]
fn a_recent_row_reads_folder_sessions_running_and_the_parent_directory() {
    let en = Strings::of(Locale::En);
    let home = Some(Path::new(HOME));
    // A git repository with sessions, one running: no "Folder".
    assert_eq!(
        recent_detail(
            &en,
            &recent("/Users/example/work/lectern", false, 3, 1),
            home
        ),
        "3 sessions · 1 running · ~/work"
    );
    // A folder without sessions.
    assert_eq!(
        recent_detail(&en, &recent("/Users/example/notes", true, 0, 0), home),
        "Folder · ~"
    );
    // A folder with one session, in the zh-CN wording; outside home the
    // directory is written in full.
    assert_eq!(
        recent_detail(
            &Strings::of(Locale::ZhCn),
            &recent("/srv/data/notes", true, 1, 0),
            home
        ),
        "文件夹 · 1 个会话 · /srv/data"
    );
    // A project at the root has no parent directory to show.
    assert_eq!(recent_detail(&en, &recent("/", true, 0, 0), None), "Folder");
}

#[test]
fn a_missing_recent_row_says_so_and_nothing_else() {
    let mut entry = recent("/Users/example/gone", true, 4, 2);
    entry.missing = true;
    assert_eq!(
        recent_detail(&Strings::of(Locale::En), &entry, Some(Path::new(HOME))),
        "Folder no longer exists"
    );
}

#[test]
fn a_discovered_row_names_a_single_agent_and_not_several() {
    let en = Strings::of(Locale::En);
    assert_eq!(
        plain(discovered_detail(
            &en,
            &discovered(&[Agent::ClaudeCode], 2, 0, 3),
            NOW
        )),
        "2 Claude Code sessions · 3 min ago"
    );
    assert_eq!(
        plain(discovered_detail(
            &en,
            &discovered(&[Agent::Codex], 1, 1, 0),
            NOW
        )),
        "1 Codex session · 1 running · just now"
    );
    assert_eq!(
        discovered_detail(
            &en,
            &discovered(&[Agent::Codex, Agent::ClaudeCode], 5, 0, 90),
            NOW
        ),
        "5 sessions · 1 hour ago"
    );
    assert_eq!(
        plain(discovered_detail(
            &Strings::of(Locale::ZhCn),
            &discovered(&[Agent::Codex], 2, 0, 3),
            NOW
        )),
        "2 个 Codex 会话 · 3 分钟前"
    );
    // An agent without a message of its own is named as it is.
    assert_eq!(
        plain(discovered_detail(
            &en,
            &discovered(&[Agent::Other(Arc::from("Aider"))], 2, 0, 3),
            NOW
        )),
        "2 Aider sessions · 3 min ago"
    );
}

#[test]
fn the_subtitle_carries_the_version_or_goes_without() {
    let en = Strings::of(Locale::En);
    assert_eq!(
        plain(subtitle(&en, Some("0.1")).as_str()),
        "Version 0.1 · Read what your agents wrote."
    );
    assert_eq!(subtitle(&en, None).as_str(), "Read what your agents wrote.");
    assert_eq!(
        plain(subtitle(&Strings::of(Locale::ZhCn), Some("0.1")).as_str()),
        "版本 0.1 · 看 agent 写了什么。"
    );
}
