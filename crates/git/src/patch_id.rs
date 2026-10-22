//! Patch IDs (docs/design/git.md「usage 用的读取」): the measure of
//! `git patch-id --stable`, implemented from git's documentation and checked
//! against git's output on fixtures, never from git's code (architecture
//! rule 8).
//!
//! A commit's diff against its first parent is split into file diffs, in
//! path order, as `git diff --no-renames` prints them; each file diff is
//! hashed with its `index` and `@@` lines, `\ No newline at end of file`
//! and every space, tab, CR and LF left out, and the hashes are added as
//! little-endian numbers, so the order of the files doesn't matter. A file
//! diff without hunks (a mode change, an empty file) is hashed together
//! with the next file diff, as git does.

use std::sync::atomic::AtomicBool;

use gix::diff::blob::sources::byte_lines;

use crate::attrs::TreeAttributes;
use crate::changes::{Blob, has_nul, kind_of, read_blob};
use crate::handle::Handle;
use crate::hunks::{self, CONTEXT, Computed, HunkKey};
use crate::refs::commit_info;
use crate::{GitError, HashKind, Mode, Oid, Repo, RepoPath, tree_diff};

/// The stable patch ID of a commit: an OID of the repository's hash
/// algorithm, equal for commits that make the same change.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PatchId(pub Oid);

/// How much one `patch_id` call reads at most (docs/design/git.md「usage
/// 用的读取」). Beyond them it fails instead of giving an ID made from part
/// of the change.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PatchLimits {
    /// Changed paths (`GitError::TooManyEntries` above it).
    pub files: usize,
    /// Bytes of one blob (`GitError::TooLarge`), the line-diff limit.
    pub file_bytes: u64,
    /// Bytes of all blobs read (`GitError::TooLarge`).
    pub total_bytes: u64,
}

impl PatchLimits {
    pub(crate) const DEFAULT: PatchLimits = PatchLimits {
        files: 5000,
        file_bytes: 16 * 1024 * 1024,
        total_bytes: 64 * 1024 * 1024,
    };
}

impl Repo {
    /// The patch ID of `commit`'s change against its first parent, as
    /// `git patch-id --stable` gives it for `git show --no-renames` with this
    /// crate's diff (Histogram, git's indent heuristic), and with an ID of
    /// its own for binary files (docs/design/git.md「usage 用的读取」). None for
    /// a root commit, a merge and a commit that changes nothing. More than
    /// 5000 changed paths give `GitError::TooManyEntries`, a blob over
    /// 16 MiB or more than 64 MiB read in all `GitError::TooLarge`. Blocks.
    pub fn patch_id(&self, commit: Oid) -> Result<Option<PatchId>, GitError> {
        self.patch_id_within(commit, &PatchLimits::DEFAULT)
    }

    pub(crate) fn patch_id_within(
        &self,
        commit: Oid,
        limits: &PatchLimits,
    ) -> Result<Option<PatchId>, GitError> {
        let handle = self.open_common()?;
        let info = commit_info(&handle, commit)?;
        let [parent] = info.parents[..] else {
            return Ok(None);
        };
        let parent_tree = commit_info(&handle, parent)?.tree;
        tree_patch_id(&handle, self.hash(), parent_tree, info.tree, limits)
    }
}

/// The patch ID of the change from tree `old` to tree `new`; None when
/// they are the same.
fn tree_patch_id(
    handle: &Handle,
    hash: HashKind,
    old: Oid,
    new: Oid,
    limits: &PatchLimits,
) -> Result<Option<PatchId>, GitError> {
    let never = AtomicBool::new(false);
    // Names git itself rejects are not walked, as in `tree_changes`.
    let walk = tree_diff::diff(handle, old, new, limits.files, &never)?;
    if walk.entries.is_empty() {
        return Ok(None);
    }
    let mut attributes = TreeAttributes::new(handle, hash, &walk.attributes)?;
    let mut reader = Reader {
        handle,
        hash,
        limits,
        read: 0,
    };
    let mut sum = vec![0u8; hash.byte_len()];
    let mut h = Stripped::new(hash);
    // Bytes in `h` not added to `sum` yet.
    let mut pending = false;
    for entry in &walk.entries {
        let path = RepoPath::new(&entry.path).ok_or_else(|| {
            GitError::Corrupt(format!(
                "tree path {:?}",
                String::from_utf8_lossy(&entry.path)
            ))
        })?;
        let binary_attr = attributes.get(&path)?.binary;
        let file = |side: Option<(Mode, Oid)>| side.map(|(mode, oid)| FileSide { mode, oid });
        let (old, new) = (file(entry.old), file(entry.new));
        // git prints a change of kind (file, symbolic link, submodule) as
        // a deletion and an addition: two file diffs.
        let parts = match (old, new) {
            (Some(o), Some(n)) if kind_of(o.mode) != kind_of(n.mode) => {
                vec![(Some(o), None), (None, Some(n))]
            }
            pair => vec![pair],
        };
        for (old, new) in parts {
            pending = true;
            // git ends a file's hash after its hunks: a file diff without
            // any (a mode change, an empty file) is hashed together with
            // the next one.
            if reader.file_diff(&mut h, &path, old, new, binary_attr)? {
                add(&mut sum, &h.finish()?);
                pending = false;
            }
        }
    }
    if pending {
        add(&mut sum, &h.finish()?);
    }
    let id = Oid::from_bytes(hash, &sum).ok_or_else(|| GitError::Corrupt("patch ID".into()))?;
    Ok(Some(PatchId(id)))
}

/// Adds `digest` to `sum` as little-endian numbers, dropping the last carry.
fn add(sum: &mut [u8], digest: &[u8]) {
    let mut carry = 0u16;
    for (s, d) in sum.iter_mut().zip(digest) {
        carry += u16::from(*s) + u16::from(*d);
        *s = carry as u8;
        carry >>= 8;
    }
}

#[derive(Clone, Copy)]
struct FileSide {
    mode: Mode,
    oid: Oid,
}

/// What one side of a file diff holds.
enum Loaded {
    Text(Vec<u8>),
    Binary,
}

struct Reader<'a> {
    handle: &'a Handle,
    hash: HashKind,
    limits: &'a PatchLimits,
    /// Blob bytes read so far.
    read: u64,
}

impl Reader<'_> {
    /// Feeds one file diff to `h`, as `git diff` prints it, without what
    /// patch IDs leave out. True when it has hunks (or is binary), after
    /// which git ends the file's hash.
    fn file_diff(
        &mut self,
        h: &mut Stripped,
        path: &RepoPath,
        old: Option<FileSide>,
        new: Option<FileSide>,
        binary_attr: bool,
    ) -> Result<bool, GitError> {
        let a = quoted(b"a/", path.as_bytes());
        let b = quoted(b"b/", path.as_bytes());
        h.line(&[b"diff --git ", &a, b" ", &b]);
        match (old, new) {
            (None, Some(n)) => h.line(&[b"new file mode ", mode_text(n.mode)]),
            (Some(o), None) => h.line(&[b"deleted file mode ", mode_text(o.mode)]),
            (Some(o), Some(n)) if o.mode != n.mode => {
                h.line(&[b"old mode ", mode_text(o.mode)]);
                h.line(&[b"new mode ", mode_text(n.mode)]);
            }
            _ => {}
        }
        if matches!((old, new), (Some(o), Some(n)) if o.oid == n.oid) {
            return Ok(false);
        }
        let old_side = old.map(|s| self.load(s, binary_attr)).transpose()?;
        let new_side = new.map(|s| self.load(s, binary_attr)).transpose()?;
        if matches!(old_side, Some(Loaded::Binary)) || matches!(new_side, Some(Loaded::Binary)) {
            // git's ID of a binary diff depends on the abbreviated `index`
            // line of the text it is given; this one hashes both full OIDs
            // (all zeros for a missing side).
            let hex = |s: Option<FileSide>| {
                s.map_or_else(|| "0".repeat(self.hash.hex_len()), |s| s.oid.to_hex())
            };
            h.line(&[b"binary ", hex(old).as_bytes(), b" ", hex(new).as_bytes()]);
            return Ok(true);
        }
        let text = |side: Option<Loaded>| match side {
            Some(Loaded::Text(bytes)) => bytes,
            _ => Vec::new(),
        };
        let (old_text, new_text) = (text(old_side), text(new_side));
        let computed = diff(old, new, &old_text, &new_text);
        // An empty file added or deleted has no `---` / `+++` lines either.
        if computed.diff.hunks.is_empty() {
            return Ok(false);
        }
        let minus: &[u8] = if old.is_some() { &a } else { b"/dev/null" };
        let plus: &[u8] = if new.is_some() { &b } else { b"/dev/null" };
        h.line(&[b"--- ", minus]);
        h.line(&[b"+++ ", plus]);
        hunk_lines(h, &old_text, &new_text, &computed);
        Ok(true)
    }

    /// A side's content: a blob's bytes, or `Subproject commit <OID>` for a
    /// submodule as git prints it.
    fn load(&mut self, side: FileSide, binary_attr: bool) -> Result<Loaded, GitError> {
        if side.mode == Mode::Gitlink {
            return Ok(Loaded::Text(
                format!("Subproject commit {}\n", side.oid.to_hex()).into_bytes(),
            ));
        }
        if binary_attr {
            return Ok(Loaded::Binary);
        }
        let bytes = match read_blob(self.handle, side.oid, self.limits.file_bytes)? {
            Blob::Bytes(bytes) => bytes,
            Blob::TooLarge(size) => return Err(GitError::TooLarge { size }),
            Blob::Missing => return Err(GitError::ObjectMissing(side.oid)),
        };
        self.read = self.read.saturating_add(bytes.len() as u64);
        if self.read > self.limits.total_bytes {
            return Err(GitError::TooLarge { size: self.read });
        }
        Ok(if has_nul(&bytes) {
            Loaded::Binary
        } else {
            Loaded::Text(bytes)
        })
    }
}

/// The line diff, from the hunk cache for two text blobs (architecture
/// rule 5); a new or deleted file is one hunk of every line.
fn diff(
    old: Option<FileSide>,
    new: Option<FileSide>,
    old_text: &[u8],
    new_text: &[u8],
) -> Computed {
    let (Some(o), Some(n)) = (old, new) else {
        return hunks::whole(old_text, new_text);
    };
    if o.mode == Mode::Gitlink {
        return hunks::compute(old_text, new_text);
    }
    let key = HunkKey::lines(o.oid, n.oid);
    if let Some(hit) = hunks::cached(&key) {
        return hit;
    }
    let computed = hunks::compute(old_text, new_text);
    hunks::remember(key, computed.clone());
    computed
}

/// The context, removed and added lines of the unified patch with three
/// lines of context, hunks closer than twice the context merged (as the
/// patch size limits count them, `hunks::patch_body_bytes`).
fn hunk_lines(h: &mut Stripped, old: &[u8], new: &[u8], computed: &Computed) {
    let old_lines: Vec<&[u8]> = byte_lines(old).collect();
    let new_lines: Vec<&[u8]> = byte_lines(new).collect();
    let hunks = &computed.diff.hunks;
    let old_len = old_lines.len() as u32;
    let context = |h: &mut Stripped, range: std::ops::Range<u32>| {
        for k in range {
            h.line(&[b" ", old_lines[k as usize]]);
        }
    };
    let mut i = 0;
    while i < hunks.len() {
        let mut j = i;
        while j + 1 < hunks.len() && hunks[j + 1].old.start - hunks[j].old.end <= 2 * CONTEXT {
            j += 1;
        }
        let first = &hunks[i];
        let lead = first.old.start.min(CONTEXT);
        context(h, first.old.start - lead..first.old.start);
        for k in i..=j {
            let hunk = &hunks[k];
            for line in hunk.old.clone() {
                h.line(&[b"-", old_lines[line as usize]]);
            }
            for line in hunk.new.clone() {
                h.line(&[b"+", new_lines[line as usize]]);
            }
            let next = if k < j {
                hunks[k + 1].old.start
            } else {
                (hunk.old.end + CONTEXT).min(old_len)
            };
            context(h, hunk.old.end..next);
        }
        i = j + 1;
    }
}

/// A hasher fed with every byte but spaces, tabs, CRs and LFs: the
/// whitespace patch IDs ignore (other control bytes count, as in git).
struct Stripped {
    hash: HashKind,
    hasher: gix::hash::Hasher,
}

impl Stripped {
    fn new(hash: HashKind) -> Stripped {
        Stripped {
            hash,
            hasher: gix::hash::hasher(hash.to_gix()),
        }
    }

    /// The digest of everything fed since the last call; starts afresh.
    fn finish(&mut self) -> Result<Vec<u8>, GitError> {
        let hasher = std::mem::replace(&mut self.hasher, gix::hash::hasher(self.hash.to_gix()));
        let digest = hasher
            .try_finalize()
            .map_err(|err| GitError::Corrupt(format!("patch ID: {err}")))?;
        Ok(digest.as_bytes().to_vec())
    }

    fn line(&mut self, parts: &[&[u8]]) {
        for part in parts {
            for run in part.split(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n')) {
                if !run.is_empty() {
                    self.hasher.update(run);
                }
            }
        }
    }
}

fn mode_text(mode: Mode) -> &'static [u8] {
    match mode {
        Mode::File => b"100644",
        Mode::Executable => b"100755",
        Mode::Symlink => b"120000",
        Mode::Gitlink => b"160000",
        // Trees never hold one.
        Mode::Special => b"0",
    }
}

/// `prefix` + `path` as `git diff` prints a path with `core.quotePath`
/// (git's default): in double quotes, with C escapes and octal bytes,
/// when the path has a control byte, `"`, `\` or a byte of 0x80 and above.
fn quoted(prefix: &[u8], path: &[u8]) -> Vec<u8> {
    let unusual = |b: u8| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80;
    let mut out = Vec::with_capacity(prefix.len() + path.len() + 2);
    if !path.iter().any(|&b| unusual(b)) {
        out.extend_from_slice(prefix);
        out.extend_from_slice(path);
        return out;
    }
    out.push(b'"');
    out.extend_from_slice(prefix);
    for &b in path {
        let escape: &[u8] = match b {
            0x07 => b"\\a",
            0x08 => b"\\b",
            b'\t' => b"\\t",
            b'\n' => b"\\n",
            0x0b => b"\\v",
            0x0c => b"\\f",
            b'\r' => b"\\r",
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b if unusual(b) => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
                continue;
            }
            _ => {
                out.push(b);
                continue;
            }
        };
        out.extend_from_slice(escape);
    }
    out.push(b'"');
    out
}

#[cfg(test)]
mod tests;
