//! FSEvents event → `RawEvent`: the root prefix stripped, the flags mapped
//! (docs/design/fs_watch.md「platform 后端」). Pure, so it is compiled and
//! unit-tested on every platform.

use lectern_fs_watch::{Flags, RawEvent};

/// `FSEventStreamEventFlags` (CoreServices `FSEvents.h`).
pub(crate) mod raw {
    pub(crate) const MUST_SCAN_SUB_DIRS: u32 = 0x0000_0001;
    pub(crate) const USER_DROPPED: u32 = 0x0000_0002;
    pub(crate) const KERNEL_DROPPED: u32 = 0x0000_0004;
    pub(crate) const EVENT_IDS_WRAPPED: u32 = 0x0000_0008;
    pub(crate) const HISTORY_DONE: u32 = 0x0000_0010;
    pub(crate) const ROOT_CHANGED: u32 = 0x0000_0020;
    pub(crate) const MOUNT: u32 = 0x0000_0040;
    pub(crate) const UNMOUNT: u32 = 0x0000_0080;
    pub(crate) const ITEM_CREATED: u32 = 0x0000_0100;
    pub(crate) const ITEM_REMOVED: u32 = 0x0000_0200;
    pub(crate) const ITEM_INODE_META_MOD: u32 = 0x0000_0400;
    pub(crate) const ITEM_RENAMED: u32 = 0x0000_0800;
    pub(crate) const ITEM_MODIFIED: u32 = 0x0000_1000;
    pub(crate) const ITEM_FINDER_INFO_MOD: u32 = 0x0000_2000;
    pub(crate) const ITEM_CHANGE_OWNER: u32 = 0x0000_4000;
    pub(crate) const ITEM_XATTR_MOD: u32 = 0x0000_8000;
    pub(crate) const ITEM_IS_FILE: u32 = 0x0001_0000;
    pub(crate) const ITEM_IS_DIR: u32 = 0x0002_0000;
    pub(crate) const ITEM_IS_SYMLINK: u32 = 0x0004_0000;
    pub(crate) const ITEM_CLONED: u32 = 0x0040_0000;
}

/// The item flags that map one to one, or many to one.
const ITEM: [(u32, Flags); 13] = [
    (raw::ITEM_IS_FILE, Flags::FILE),
    (raw::ITEM_IS_DIR, Flags::DIR),
    (raw::ITEM_IS_SYMLINK, Flags::SYMLINK),
    (raw::ITEM_CREATED, Flags::CREATED),
    (raw::ITEM_REMOVED, Flags::REMOVED),
    (raw::ITEM_RENAMED, Flags::RENAMED),
    (raw::ITEM_MODIFIED, Flags::MODIFIED),
    (raw::ITEM_CLONED, Flags::MODIFIED),
    (raw::ITEM_INODE_META_MOD, Flags::META),
    (raw::ITEM_CHANGE_OWNER, Flags::META),
    (raw::ITEM_XATTR_MOD, Flags::META),
    (raw::ITEM_FINDER_INFO_MOD, Flags::META),
    (raw::MUST_SCAN_SUB_DIRS, Flags::RESCAN),
];

/// Converts one event. `root` is the canonical path the stream watches (no
/// trailing `/`, except for `/` itself), `path` the event's path as FSEvents
/// reports it. Returns None for the events fs_watch drops
/// (`EventIdsWrapped`, `HistoryDone`).
pub(crate) fn event(root: &[u8], path: &[u8], flags: u32) -> Option<RawEvent> {
    if flags & (raw::EVENT_IDS_WRAPPED | raw::HISTORY_DONE) != 0 {
        return None;
    }
    let relative = strip(root, path);
    if flags & raw::ROOT_CHANGED != 0 {
        return Some(at(b"", Flags::ROOT_CHANGED));
    }
    if flags & (raw::MOUNT | raw::UNMOUNT) != 0 {
        return Some(match relative {
            // A volume mounted or unmounted below the root: what is under
            // the mount point changed as a whole, so its directory is
            // rescanned.
            Some(relative) if !relative.is_empty() => at(parent(relative), Flags::RESCAN),
            // The mount point is the root or above it: unmounting takes the
            // target with it, mounting over it hides what was there.
            _ if flags & raw::UNMOUNT != 0 => at(b"", Flags::UNMOUNTED),
            _ => at(b"", Flags::RESCAN),
        });
    }
    let mut mapped = Flags::default();
    for (bit, flag) in ITEM {
        if flags & bit != 0 {
            mapped |= flag;
        }
    }
    if flags & raw::MUST_SCAN_SUB_DIRS != 0
        && flags & (raw::USER_DROPPED | raw::KERNEL_DROPPED) != 0
    {
        mapped |= Flags::DROPPED;
    }
    match relative {
        Some(relative) => Some(at(relative, mapped)),
        // Dropped events can be reported against a path above the root;
        // what was lost is somewhere in the target, so all of it is
        // rescanned.
        None if mapped.contains(Flags::RESCAN) => Some(at(b"", mapped)),
        // Only a change of the root itself (or of a directory above it)
        // gives a path outside the root.
        None => Some(at(b"", Flags::ROOT_CHANGED)),
    }
}

fn at(path: &[u8], flags: Flags) -> RawEvent {
    RawEvent {
        path: path.into(),
        flags,
    }
}

/// `path` relative to `root`: empty for the root itself, None outside it.
/// Trailing slashes are ignored on both.
pub(crate) fn strip<'a>(root: &[u8], path: &'a [u8]) -> Option<&'a [u8]> {
    let root = trim(root);
    let path = trim(path);
    if path == root {
        return Some(b"");
    }
    match path.strip_prefix(root)?.split_first() {
        Some((b'/', rest)) if !rest.is_empty() => Some(rest),
        _ => None,
    }
}

fn trim(mut path: &[u8]) -> &[u8] {
    while let Some(rest) = path.strip_suffix(b"/") {
        path = rest;
    }
    path
}

fn parent(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&byte| byte == b'/') {
        Some(slash) => &path[..slash],
        None => b"",
    }
}
