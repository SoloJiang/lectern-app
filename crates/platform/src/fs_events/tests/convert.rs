//! `convert`: FSEvents paths and flags → `RawEvent`. Runs on every platform.

use lectern_fs_watch::{Flags, RawEvent};

use crate::fs_events::convert::raw::*;
use crate::fs_events::convert::{event, strip};

const ROOT: &[u8] = b"/private/tmp/w";

fn ev(path: &str, flags: Flags) -> Option<RawEvent> {
    Some(RawEvent {
        path: path.as_bytes().into(),
        flags,
    })
}

#[test]
fn fsevents_convert_strips_the_root_prefix() {
    assert_eq!(strip(ROOT, b"/private/tmp/w"), Some(&b""[..]));
    assert_eq!(strip(ROOT, b"/private/tmp/w/"), Some(&b""[..]));
    assert_eq!(strip(ROOT, b"/private/tmp/w/a"), Some(&b"a"[..]));
    assert_eq!(
        strip(ROOT, b"/private/tmp/w/a/b.txt"),
        Some(&b"a/b.txt"[..])
    );
    assert_eq!(strip(ROOT, b"/private/tmp/w/a/"), Some(&b"a"[..]));
    // A sibling sharing the prefix, a parent, the unresolved spelling.
    assert_eq!(strip(ROOT, b"/private/tmp/wx/a"), None);
    assert_eq!(strip(ROOT, b"/private/tmp"), None);
    assert_eq!(strip(ROOT, b"/tmp/w/a"), None);
    // Watching `/` itself.
    assert_eq!(strip(b"/", b"/a/b"), Some(&b"a/b"[..]));
    assert_eq!(strip(b"/", b"/"), Some(&b""[..]));
    // Bytes that are not UTF-8 pass through.
    assert_eq!(
        strip(ROOT, b"/private/tmp/w/\xff\xfe"),
        Some(&b"\xff\xfe"[..])
    );
}

#[test]
fn fsevents_convert_maps_item_flags() {
    let cases: &[(u32, Flags)] = &[
        (ITEM_IS_FILE | ITEM_CREATED, Flags::FILE | Flags::CREATED),
        (ITEM_IS_DIR | ITEM_REMOVED, Flags::DIR | Flags::REMOVED),
        (
            ITEM_IS_SYMLINK | ITEM_RENAMED,
            Flags::SYMLINK | Flags::RENAMED,
        ),
        (ITEM_IS_FILE | ITEM_MODIFIED, Flags::FILE | Flags::MODIFIED),
        (ITEM_IS_FILE | ITEM_CLONED, Flags::FILE | Flags::MODIFIED),
        (ITEM_INODE_META_MOD, Flags::META),
        (ITEM_CHANGE_OWNER, Flags::META),
        (ITEM_XATTR_MOD, Flags::META),
        (ITEM_FINDER_INFO_MOD, Flags::META),
        (MUST_SCAN_SUB_DIRS, Flags::RESCAN),
        (
            MUST_SCAN_SUB_DIRS | USER_DROPPED,
            Flags::RESCAN | Flags::DROPPED,
        ),
        (
            MUST_SCAN_SUB_DIRS | KERNEL_DROPPED,
            Flags::RESCAN | Flags::DROPPED,
        ),
        // No item flag at all: still "this path may have changed".
        (0, Flags::default()),
        // Flags fs_watch has no use for (OwnEvent, IsHardlink) are dropped.
        (ITEM_IS_FILE | 0x0008_0000 | 0x0010_0000, Flags::FILE),
    ];
    for &(raw, flags) in cases {
        assert_eq!(
            event(ROOT, b"/private/tmp/w/a/b", raw),
            ev("a/b", flags),
            "flags {raw:#x}"
        );
    }
}

#[test]
fn fsevents_convert_root_changed_is_reported_on_the_target() {
    assert_eq!(event(ROOT, ROOT, ROOT_CHANGED), ev("", Flags::ROOT_CHANGED));
    // Whatever path FSEvents gives with it.
    assert_eq!(
        event(ROOT, b"/private/tmp", ROOT_CHANGED),
        ev("", Flags::ROOT_CHANGED)
    );
    // A path outside the root without RootChanged: only a change of the
    // root itself, or of a directory above it, gives one.
    assert_eq!(
        event(ROOT, b"/private/tmp/w2", ITEM_IS_DIR | ITEM_RENAMED),
        ev("", Flags::ROOT_CHANGED)
    );
}

#[test]
fn fsevents_convert_dropped_events_outside_the_root_rescan_the_target() {
    assert_eq!(
        event(ROOT, b"/", MUST_SCAN_SUB_DIRS | USER_DROPPED),
        ev("", Flags::RESCAN | Flags::DROPPED)
    );
    assert_eq!(
        event(
            ROOT,
            b"/private/tmp/w/deep",
            MUST_SCAN_SUB_DIRS | KERNEL_DROPPED
        ),
        ev("deep", Flags::RESCAN | Flags::DROPPED)
    );
}

#[test]
fn fsevents_convert_mounts_and_unmounts() {
    // A volume mounted below the root: rescan the directory holding the
    // mount point.
    assert_eq!(
        event(ROOT, b"/private/tmp/w/a/vol", MOUNT),
        ev("a", Flags::RESCAN)
    );
    assert_eq!(
        event(ROOT, b"/private/tmp/w/vol", MOUNT),
        ev("", Flags::RESCAN)
    );
    assert_eq!(
        event(ROOT, b"/private/tmp/w/a/vol", UNMOUNT),
        ev("a", Flags::RESCAN)
    );
    // The target's own volume.
    assert_eq!(event(ROOT, ROOT, UNMOUNT), ev("", Flags::UNMOUNTED));
    assert_eq!(
        event(ROOT, b"/Volumes/x", UNMOUNT),
        ev("", Flags::UNMOUNTED)
    );
}

#[test]
fn fsevents_convert_drops_wrapped_ids_and_history_done() {
    assert_eq!(event(ROOT, ROOT, EVENT_IDS_WRAPPED), None);
    assert_eq!(event(ROOT, b"/", HISTORY_DONE), None);
}
