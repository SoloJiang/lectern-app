//! The menu bar (docs/design/app.md「界面」「实际验证」). GPUI's test platform
//! keeps the menus `set_menus` hands over, so these tests run the real
//! `window::set_menus` and read the result back; nothing here makes a native
//! menu, and the AppKit half (the key equivalents, the greying) is not covered.

use gpui::{
    Action, KeyBindingContextPredicate, Keystroke, OsAction, OwnedMenu, OwnedMenuItem,
    TestAppContext,
};
use lectern_i18n::{Locale, Strings};
use lectern_review_view::actions as review;

use crate::menu;
use crate::window;

/// One action item: title, action name, and the OS action's name.
type Item = (String, &'static str, Option<&'static str>);

fn os_name(os: OsAction) -> &'static str {
    match os {
        OsAction::Cut => "cut",
        OsAction::Copy => "copy",
        OsAction::Paste => "paste",
        OsAction::SelectAll => "select_all",
        OsAction::Undo => "undo",
        OsAction::Redo => "redo",
    }
}

/// A menu's action items, separators left out.
fn items(menu: &OwnedMenu) -> Vec<Item> {
    menu.items
        .iter()
        .filter_map(|item| match item {
            OwnedMenuItem::Action {
                name,
                action,
                os_action,
                ..
            } => Some((name.clone(), action.name(), os_action.map(os_name))),
            _ => None,
        })
        .collect()
}

fn installed(locale: Locale, cx: &mut TestAppContext) -> Vec<OwnedMenu> {
    cx.update(|cx| {
        window::set_menus(cx, Strings::of(locale));
        cx.get_menus().expect("the test platform keeps the menus")
    })
}

fn row(title: &str, action: &'static str, os: Option<&'static str>) -> Item {
    (title.to_owned(), action, os)
}

#[gpui::test]
fn menu_bar_order_and_items_in_english(cx: &mut TestAppContext) {
    let menus = installed(Locale::En, cx);
    let names: Vec<_> = menus.iter().map(|m| m.name.to_string()).collect();
    assert_eq!(
        names,
        ["Lectern", "File", "Edit", "View", "Review", "Window"]
    );
    assert_eq!(
        items(&menus[1]),
        [
            row("Open Folder…", "workspace::OpenFolder", None),
            row("Close Project", "workspace::CloseProject", None),
            row("Close Window", "workspace::CloseWindow", None),
        ]
    );
    assert_eq!(
        items(&menus[2]),
        [
            row("Undo", "input::Undo", Some("undo")),
            row("Redo", "input::Redo", Some("redo")),
            row("Cut", "input::Cut", Some("cut")),
            row("Copy", "input::Copy", Some("copy")),
            row("Paste", "input::Paste", Some("paste")),
            row("Select All", "input::SelectAll", Some("select_all")),
        ]
    );
    // The View menu is as it was.
    assert_eq!(
        items(&menus[3]),
        [
            row("Unified / Side by Side", "review::ToggleDiffMode", None),
            row("Refresh", "review::Refresh", None),
        ]
    );
    assert_eq!(
        items(&menus[4]),
        [
            row("Next Change", "review::NextHunk", None),
            row("Previous Change", "review::PreviousHunk", None),
            row("Mark Reviewed", "review::MarkSeenAndNext", None),
            row("Add Note", "review::AddNote", None),
            row("Send Notes", "review::SendNotes", None),
        ]
    );
    assert_eq!(
        items(&menus[5]),
        [
            row("Minimize", "lectern::Minimize", None),
            row("Zoom", "lectern::Zoom", None),
            row("Bring All to Front", "lectern::BringAllToFront", None),
        ]
    );
}

/// Same order, same actions, other words: every title is the zh-CN one.
#[gpui::test]
fn menu_bar_in_chinese_has_the_same_actions(cx: &mut TestAppContext) {
    let en = installed(Locale::En, cx);
    let zh = installed(Locale::ZhCn, cx);
    assert_eq!(en.len(), zh.len());
    let names: Vec<_> = zh.iter().map(|m| m.name.to_string()).collect();
    assert_eq!(names, ["Lectern", "文件", "编辑", "显示", "审阅", "窗口"]);
    for (en, zh) in en.iter().zip(&zh) {
        let (en_items, zh_items) = (items(en), items(zh));
        assert_eq!(en_items.len(), zh_items.len(), "{}", en.name);
        for (en, zh) in en_items.iter().zip(&zh_items) {
            assert_eq!((en.1, en.2), (zh.1, zh.2), "{}", zh.0);
            assert!(!zh.0.is_empty());
            assert_ne!(en.0, zh.0, "{} is not translated", en.1);
        }
    }
    assert_eq!(
        items(&zh[4])
            .iter()
            .map(|i| i.0.as_str())
            .collect::<Vec<_>>(),
        ["下一处", "上一处", "标记已看", "加审阅意见", "发送审阅意见"]
    );
}

/// The shortcuts the menu shows are the keymap's when `set_menus` runs. J, K,
/// V and C must have no binding then: with one, the platform would give the
/// item a bare key equivalent that steals typing from the note editor (the
/// `menu` module comment). Every binding that is there has a modifier key.
#[gpui::test]
fn the_keymap_set_menus_reads_has_no_bare_key(cx: &mut TestAppContext) {
    let menus = installed(Locale::En, cx);
    let shown = cx.update(|cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let mut shown = Vec::new();
        for menu in &menus {
            for item in &menu.items {
                if let OwnedMenuItem::Action { action, .. } = item {
                    // What the macOS platform reads: the first binding.
                    let first = keymap
                        .bindings_for_action(action.as_ref())
                        .next()
                        .map(|binding| binding.keystrokes()[0].inner().clone());
                    if let Some(keystroke) = &first {
                        assert!(
                            keystroke.modifiers.modified(),
                            "{} would get the bare key {:?}",
                            action.name(),
                            keystroke.unparse()
                        );
                    }
                    shown.push((action.name(), first.map(|k| k.unparse())));
                }
            }
        }
        shown
    });
    let key = |name: &str| {
        shown
            .iter()
            .find(|(action, _)| *action == name)
            .unwrap_or_else(|| panic!("no menu item for {name}"))
            .1
            .clone()
    };
    for name in [
        "review::NextHunk",
        "review::PreviousHunk",
        "review::MarkSeenAndNext",
        "review::AddNote",
    ] {
        assert_eq!(key(name), None, "{name}");
    }
    for (name, keys) in [
        ("workspace::OpenFolder", "cmd-o"),
        ("workspace::CloseWindow", "cmd-w"),
        ("workspace::CloseProject", ""),
        ("input::Undo", "cmd-z"),
        ("input::Redo", "cmd-shift-z"),
        ("input::Cut", "cmd-x"),
        ("input::Copy", "cmd-c"),
        ("input::Paste", "cmd-v"),
        ("input::SelectAll", "cmd-a"),
        ("review::SendNotes", "cmd-enter"),
        ("review::Refresh", "cmd-r"),
        ("lectern::Minimize", "cmd-m"),
    ] {
        let want = (!keys.is_empty()).then(|| keys.to_owned());
        assert_eq!(key(name), want, "{name}");
    }
}

/// The early bindings parse, keep the owner's contexts and add nothing for
/// the unmodified keys of the review screen.
#[test]
fn early_bindings_are_valid_and_modified() {
    let bindings = menu::key_bindings();
    assert!(!bindings.is_empty());
    for (keys, action, context) in &bindings {
        let keystroke = Keystroke::parse(keys).unwrap_or_else(|e| panic!("{keys}: {e:?}"));
        assert!(keystroke.modifiers.modified(), "{keys} has no modifier");
        if let Some(context) = context {
            KeyBindingContextPredicate::parse(context)
                .unwrap_or_else(|e| panic!("{context}: {e:?}"));
        }
        for bare in [
            review::NextHunk.name(),
            review::PreviousHunk.name(),
            review::MarkSeenAndNext.name(),
            review::AddNote.name(),
        ] {
            assert_ne!(action.name(), bare);
        }
    }
    // Same predicate as the review screen gives ⌘↩: not a looser one.
    let owner = review::key_bindings()
        .into_iter()
        .find(|(_, action, _)| action.name() == "review::SendNotes")
        .unwrap();
    let ours = bindings
        .iter()
        .find(|(_, action, _)| action.name() == "review::SendNotes")
        .unwrap();
    assert_eq!((ours.0, ours.2), (owner.0, owner.2));
    assert!(ours.2.is_some());
    // The Edit keys bind in the text box context only.
    for (_, action, context) in &bindings {
        if action.name().starts_with("input::") {
            assert_eq!(*context, Some("Input"), "{}", action.name());
        }
    }
}
