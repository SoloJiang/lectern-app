//! 权限请求里的改动预览（docs/design/agents.md「ACP 客户端」的「改动预览」）：工具调用内容里的 diff
//! 用 `lectern_git::line_diff` 算成 `EditPreview`。
//!
//! 算出来的行放在至多 [`CACHE_MAX`] 项的 LRU 里，键是两侧内容的 git blob OID（架构规则 5）：同一份改动
//! 再来一次（权限请求重发、两次请求改同一处）不重算。纯计算，不做 IO、不取引擎的锁；LRU 有自己的锁，
//! 只在查和放的瞬间持有，所以算 diff 的时候没有锁。

use std::path::Path;
use std::sync::{Arc, Mutex};

use lectern_git::{FileDiff, HashKind, Oid, line_diff};
use lectern_services::conversation::{EditPreview, PreviewRow, RowKind};

use super::live::clip_bytes;
use super::wire::DiffContent;

/// LRU 的项数。
const CACHE_MAX: usize = 64;
/// 每个文件的行数上限（conversation 设计「服务接口」的 `EditPreview::rows`），其余计入 `omitted`。
const ROWS_MAX: usize = 120;
/// 增删行前后的上下文行数。
const CONTEXT: usize = 3;
/// `line_diff` 接受的单侧内容上限；超过时不算 diff，见 [`Shape::unread`]。
const SIDE_MAX: usize = 2 << 20;
/// 一行的字节数上限（一行很长的压缩文件不让一张卡片撑到几十 MiB）。
const ROW_BYTES: usize = 1 << 10;
/// 路径的字节数上限。
const PATH_BYTES: usize = 4 << 10;

/// 两侧内容的 blob OID。内容相同的改动不论出自哪个文件，行都一样。
type Key = (Oid, Oid);

/// 一个文件的改动除路径和是否新文件之外的部分：只由两侧的内容决定。
struct Shape {
    added: u32,
    removed: u32,
    rows: Arc<[PreviewRow]>,
    omitted: u32,
}

/// 至多 [`CACHE_MAX`] 项的 LRU，最近用过的在最后。
#[derive(Default)]
pub(crate) struct Previews {
    cache: Mutex<Vec<(Key, Arc<Shape>)>>,
}

impl Previews {
    /// 请求里的改动变成预览。没有改动时为 None（`PermissionDetail::None`）。
    pub(crate) fn edits(&self, diffs: &[DiffContent]) -> Option<Arc<[EditPreview]>> {
        if diffs.is_empty() {
            return None;
        }
        let previews: Vec<EditPreview> = diffs
            .iter()
            .map(|diff| {
                let old = diff.old.as_deref().unwrap_or_default();
                let shape = self.shape(old, &diff.new);
                EditPreview {
                    path: Path::new(clip_bytes(&diff.path, PATH_BYTES).0).into(),
                    new_file: diff.old.is_none(),
                    added: shape.added,
                    removed: shape.removed,
                    rows: shape.rows.clone(),
                    omitted: shape.omitted,
                }
            })
            .collect();
        Some(previews.into())
    }

    fn shape(&self, old: &str, new: &str) -> Arc<Shape> {
        if old.len() > SIDE_MAX || new.len() > SIDE_MAX {
            return Arc::new(Shape::unread(old, new));
        }
        // SHA-1 blobs: the key only has to tell contents apart, and every caller uses the same kind.
        let key = (
            Oid::for_blob(HashKind::Sha1, old.as_bytes()),
            Oid::for_blob(HashKind::Sha1, new.as_bytes()),
        );
        if let Some(hit) = self.get(&key) {
            return hit;
        }
        let shape = Arc::new(Shape::of(old, new));
        self.put(key, shape.clone());
        shape
    }

    fn get(&self, key: &Key) -> Option<Arc<Shape>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let at = cache.iter().position(|(k, _)| k == key)?;
        let entry = cache.remove(at);
        let shape = entry.1.clone();
        cache.push(entry);
        Some(shape)
    }

    fn put(&self, key: Key, shape: Arc<Shape>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // Another thread may have put the same key while this one computed.
        cache.retain(|(k, _)| *k != key);
        cache.push((key, shape));
        if cache.len() > CACHE_MAX {
            cache.remove(0);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Shape {
    fn of(old: &str, new: &str) -> Shape {
        match line_diff(old.as_bytes(), new.as_bytes(), SIDE_MAX as u64) {
            Some(diff) => Shape::from_diff(old, new, &diff),
            None => Shape::unread(old, new),
        }
    }

    /// 内容太大没有算 diff：增删行数按两侧的行数记，一行也不显示（全部计入 `omitted`）。
    fn unread(old: &str, new: &str) -> Shape {
        let added = lines_of(new);
        let removed = lines_of(old);
        Shape {
            added,
            removed,
            rows: Arc::from([]),
            omitted: added.saturating_add(removed),
        }
    }

    /// 单侧各行加上下文：同 unified diff，增删行各自成块，块与块相隔不到 `2 * CONTEXT` 行时合并，
    /// 先删后增。上下文取新的一侧。
    fn from_diff(old: &str, new: &str, diff: &FileDiff) -> Shape {
        let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
        let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
        let mut out = Rows::default();
        let mut at = 0usize; // The new line after the previous hunk.
        let mut first = true;
        for hunk in diff.hunks.iter() {
            let start = hunk.new.start as usize;
            let gap = start.saturating_sub(at);
            if first {
                out.context(&new_lines[start - gap.min(CONTEXT)..start]);
            } else if gap <= 2 * CONTEXT {
                out.context(&new_lines[at..start]);
            } else {
                out.context(&new_lines[at..at + CONTEXT]);
                out.context(&new_lines[start - CONTEXT..start]);
            }
            first = false;
            for line in &old_lines[hunk.old.start as usize..hunk.old.end as usize] {
                out.push(RowKind::Removed, line);
            }
            for line in &new_lines[start..hunk.new.end as usize] {
                out.push(RowKind::Added, line);
            }
            at = hunk.new.end as usize;
        }
        if !first {
            let end = new_lines.len().min(at + CONTEXT);
            out.context(&new_lines[at.min(end)..end]);
        }
        let omitted = u32::try_from(out.total - out.rows.len()).unwrap_or(u32::MAX);
        Shape {
            added: diff.added,
            removed: diff.removed,
            rows: out.rows.into(),
            omitted,
        }
    }
}

/// 逐行收集，只保留前 [`ROWS_MAX`] 行，数下总行数。
#[derive(Default)]
struct Rows {
    rows: Vec<PreviewRow>,
    total: usize,
}

impl Rows {
    fn push(&mut self, kind: RowKind, line: &str) {
        self.total += 1;
        if self.rows.len() < ROWS_MAX {
            let line = line.strip_suffix('\n').unwrap_or(line);
            let line = line.strip_suffix('\r').unwrap_or(line);
            self.rows.push(PreviewRow {
                kind,
                text: clip_bytes(line, ROW_BYTES).0.into(),
            });
        }
    }

    fn context(&mut self, lines: &[&str]) {
        for line in lines {
            self.push(RowKind::Context, line);
        }
    }
}

/// 行数：以 `\n` 结尾的每行算一行，最后没有 `\n` 的也算（同 `line_diff`）。
fn lines_of(text: &str) -> u32 {
    let newlines = text.bytes().filter(|b| *b == b'\n').count();
    let tail = usize::from(!text.is_empty() && !text.ends_with('\n'));
    u32::try_from(newlines + tail).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(path: &str, old: Option<&str>, new: &str) -> DiffContent {
        DiffContent {
            path: path.into(),
            old: old.map(str::to_owned),
            new: new.into(),
        }
    }

    fn preview_of(old: Option<&str>, new: &str) -> EditPreview {
        let previews = Previews::default();
        let all = previews.edits(&[diff("/w/a.rs", old, new)]).unwrap();
        all[0].clone()
    }

    fn render(preview: &EditPreview) -> Vec<String> {
        preview
            .rows
            .iter()
            .map(|row| {
                let sign = match row.kind {
                    RowKind::Context => ' ',
                    RowKind::Added => '+',
                    RowKind::Removed => '-',
                };
                format!("{sign}{}", row.text)
            })
            .collect()
    }

    fn numbered(range: std::ops::Range<u32>) -> String {
        range.map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn a_change_gets_three_lines_of_context_and_removed_lines_come_first() {
        let old = numbered(0..20);
        let new = old.replace("line 10\n", "ten\n");
        let preview = preview_of(Some(&old), &new);
        assert_eq!((preview.added, preview.removed, preview.omitted), (1, 1, 0));
        assert!(!preview.new_file);
        assert_eq!(&*preview.path, Path::new("/w/a.rs"));
        assert_eq!(
            render(&preview),
            [
                " line 7", " line 8", " line 9", "-line 10", "+ten", " line 11", " line 12",
                " line 13"
            ]
        );
    }

    #[test]
    fn close_hunks_share_their_context_and_far_ones_do_not() {
        let old = numbered(0..40);
        // Two lines between the changes (merged), then thirty (split).
        let new = old
            .replace("line 5\n", "five\n")
            .replace("line 8\n", "eight\n")
            .replace("line 38\n", "late\n");
        let preview = preview_of(Some(&old), &new);
        let rows = render(&preview);
        assert_eq!(
            rows[..13],
            [
                " line 2", " line 3", " line 4", "-line 5", "+five", " line 6", " line 7",
                "-line 8", "+eight", " line 9", " line 10", " line 11", " line 35"
            ],
            "{rows:?}"
        );
        assert_eq!(
            rows[13..],
            [" line 36", " line 37", "-line 38", "+late", " line 39"]
        );
    }

    #[test]
    fn a_new_file_is_all_added_and_a_deletion_all_removed() {
        let created = preview_of(None, "one\ntwo");
        assert!(created.new_file);
        assert_eq!((created.added, created.removed), (2, 0));
        assert_eq!(render(&created), ["+one", "+two"]);
        let emptied = preview_of(Some("one\ntwo\n"), "");
        assert!(!emptied.new_file);
        assert_eq!((emptied.added, emptied.removed), (0, 2));
        assert_eq!(render(&emptied), ["-one", "-two"]);
        let same = preview_of(Some("a\n"), "a\n");
        assert_eq!((same.added, same.removed), (0, 0));
        assert!(same.rows.is_empty());
    }

    #[test]
    fn rows_beyond_the_cap_are_counted_and_long_lines_are_clipped() {
        let created = preview_of(None, &"x\n".repeat(ROWS_MAX + 30));
        assert_eq!(created.rows.len(), ROWS_MAX);
        assert_eq!(created.omitted, 30);
        assert_eq!(created.added as usize, ROWS_MAX + 30);
        let long = "é".repeat(ROW_BYTES);
        let created = preview_of(None, &format!("{long}\r\n"));
        assert_eq!(created.rows[0].text.len(), ROW_BYTES);
        assert!(!created.rows[0].text.ends_with('\r'));
    }

    #[test]
    fn a_side_over_the_limit_is_counted_not_shown() {
        let big = "a\n".repeat(SIDE_MAX / 2 + 1);
        let preview = preview_of(Some("old\n"), &big);
        assert!(preview.rows.is_empty());
        assert_eq!(preview.removed, 1);
        assert_eq!(preview.added as usize, SIDE_MAX / 2 + 1);
        assert_eq!(preview.omitted, preview.added + 1);
    }

    #[test]
    fn the_cache_is_keyed_by_content_and_keeps_sixty_four_most_recent() {
        let previews = Previews::default();
        let one = previews.edits(&[diff("/a", Some("x\n"), "y\n")]).unwrap();
        // The same contents under another path are one entry with the same rows.
        let two = previews.edits(&[diff("/b", Some("x\n"), "y\n")]).unwrap();
        assert_eq!(previews.len(), 1);
        assert!(Arc::ptr_eq(&one[0].rows, &two[0].rows));
        assert_ne!(one[0].path, two[0].path);
        for n in 0..CACHE_MAX {
            previews.edits(&[diff("/c", None, &format!("{n}\n"))]);
        }
        assert_eq!(previews.len(), CACHE_MAX);
        // The first entry was used least recently; a hit moves an entry to the end.
        let again = previews.edits(&[diff("/a", Some("x\n"), "y\n")]).unwrap();
        assert!(!Arc::ptr_eq(&one[0].rows, &again[0].rows), "it was evicted");
        let kept = previews.edits(&[diff("/c", None, "63\n")]).unwrap();
        previews.edits(&[diff("/d", None, "fresh\n")]);
        let after = previews.edits(&[diff("/c", None, "63\n")]).unwrap();
        assert!(Arc::ptr_eq(&kept[0].rows, &after[0].rows));
        assert!(previews.edits(&[]).is_none());
    }
}
