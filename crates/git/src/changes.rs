//! Change sets: what differs between a base tree and a target, per file,
//! with each file's diff under the limits (docs/design/git.md「改动集」,
//! 「数据结构」, 「上限与截断」). Tree ↔ tree here; the worktree comparison
//! (step 3) feeds the same assembly with worktree contents.

use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::attrs::{Attrs, TreeAttributes};
use crate::handle::Handle;
use crate::hunks::{self, Computed, FileDiff, HunkKey};
use crate::refs::missing_or;
use crate::rename::{self, Candidate, Signature};
use crate::{Content, GitError, Mode, Oid, Repo, RepoPath, Unavailable, WorktreeId, tree_diff};

/// The limits of one comparison (docs/design/git.md「上限与截断」). Tree
/// diffs use the line-diff, patch, total and entry limits; the worktree
/// comparison also uses the read, untracked and retained-content limits.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Limits {
    /// A side larger than this isn't line-diffed: `Detail::TooLarge`. 16 MiB.
    pub diff_bytes: u64,
    /// A file whose unified patch (3 lines of context) is larger is
    /// `Detail::Truncated`. 1 MiB.
    pub patch_bytes: u64,
    /// So is one with more changed (added + removed) lines. 20 000.
    pub patch_lines: u32,
    /// Files after this many (in path order, every kind counted) are
    /// `Detail::Listed`. 5000.
    pub total_files: usize,
    /// So are files after the patches so far reach this many bytes. 50 MB.
    pub total_patch_bytes: u64,
    /// A worktree file larger than this isn't read: `Content::Unhashed`. 64 MiB.
    pub read_bytes: u64,
    /// Untracked files listed at most. 20 000.
    pub untracked: usize,
    /// Worktree content the change set keeps for `texts`; past it, new sides
    /// are `Unavailable::NotRetained`. 32 MiB.
    pub retained_bytes: u64,
    /// Changed paths (and index entries) at most: `GitError::TooManyEntries`
    /// above it. 1 000 000.
    pub entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            diff_bytes: 16 * 1024 * 1024,
            patch_bytes: 1024 * 1024,
            patch_lines: 20_000,
            total_files: 5000,
            total_patch_bytes: 50_000_000,
            read_bytes: 64 * 1024 * 1024,
            untracked: 20_000,
            retained_bytes: 32 * 1024 * 1024,
            entries: 1_000_000,
        }
    }
}

/// What the base was compared with.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    Tree(Oid),
    Worktree(WorktreeId),
}

/// Counts over a change set.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Totals {
    /// `files().len()`.
    pub files: usize,
    /// Lines added and removed where known (`Text` and `Truncated`).
    pub added: u64,
    pub removed: u64,
    /// Files whose `Detail` is `Truncated`, `TooLarge`, `Listed`.
    pub truncated: usize,
    pub too_large: usize,
    pub listed: usize,
    /// Files whose new side is `Unavailable::NotRetained`.
    pub not_retained: usize,
    /// The untracked-file limit was reached (worktree comparison).
    pub untracked_capped: bool,
    /// Paths that break the `RepoPath` invariants, raw bytes: not read and
    /// not in `files` ("路径无效，未读取").
    pub invalid_paths: Arc<[Arc<[u8]>]>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    pub status: Status,
    /// The new path; the old path for a deletion.
    pub path: RepoPath,
    /// The base side; None when added.
    pub old: Option<Side>,
    /// The target side; None when deleted.
    pub new: Option<Side>,
    pub detail: Detail,
    /// `.gitattributes` `linguist-generated`.
    pub generated: bool,
    /// The new side is a worktree file the index doesn't have. Only on `Added`.
    pub untracked: bool,
    /// The index entry, when it differs from the worktree side (worktree
    /// comparison only).
    pub staged: Option<Staged>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Staged {
    pub oid: Oid,
    pub mode: Mode,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Status {
    Added,
    Deleted,
    Modified,
    /// Between a regular file, a symbolic link, a submodule and `Mode::Special`.
    TypeChanged,
    /// `similarity` is git's percentage; 100 for an exact rename.
    Renamed {
        from: RepoPath,
        similarity: u8,
    },
    /// Unresolved conflict entries in the index (worktree comparison).
    Conflicted,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Side {
    pub mode: Mode,
    pub content: Content,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Detail {
    Text(FileDiff),
    /// Over the per-file patch limit: only the header and line counts.
    Truncated {
        added: u32,
        removed: u32,
    },
    Binary {
        old_size: Option<u64>,
        new_size: Option<u64>,
    },
    /// A side is over the line-diff limit: not compared, counts unknown.
    TooLarge,
    /// Over the total limit: only the path and the kind of change.
    Listed,
    /// Same content, only the mode changed.
    ModeOnly,
    /// The worktree equals the base; only the index differs.
    StagedOnly,
    /// A side is `Unavailable` (except `NotRetained`) or a submodule.
    NoDiff,
}

/// The texts of a file's two sides, for [`rows`](crate::rows) and the view.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileTexts {
    pub old: SideText,
    pub new: SideText,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SideText {
    /// The side doesn't exist: the old side of an addition, the new side of
    /// a deletion.
    Absent,
    Text(Text),
    /// Over the line-diff limit; not read.
    TooLarge {
        size: u64,
    },
    /// Worktree content over the retained-content limit: "内容过多，未保留".
    /// Not reread by path: it may no longer be what was compared.
    NotRetained,
    /// A submodule, or content that is `Unavailable` or `Unhashed`.
    Unavailable,
}

/// A file's bytes with their line boundaries. Lines end after `\n`; a last
/// line without one is a line, as in the hunks.
#[derive(Clone, PartialEq, Eq)]
pub struct Text {
    bytes: Arc<[u8]>,
    /// Start of each line, then `bytes.len()`.
    starts: Arc<[usize]>,
}

impl Text {
    pub fn new(bytes: Arc<[u8]>) -> Text {
        let mut starts = vec![0];
        starts.extend(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i + 1),
        );
        if starts.last() != Some(&bytes.len()) {
            starts.push(bytes.len());
        }
        Text {
            bytes,
            starts: starts.into(),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn line_count(&self) -> u32 {
        u32::try_from(self.starts.len() - 1).unwrap_or(u32::MAX)
    }

    /// Line `n` (0-based) without its `\n`; empty past the end.
    pub fn line(&self, n: u32) -> &[u8] {
        let n = n as usize;
        let (Some(&start), Some(&end)) = (self.starts.get(n), self.starts.get(n + 1)) else {
            return &[];
        };
        let line = &self.bytes[start..end];
        line.strip_suffix(b"\n").unwrap_or(line)
    }

    /// Whether the last line ends with `\n` (git's "No newline at end of
    /// file" when not). True for empty text.
    pub fn ends_with_newline(&self) -> bool {
        self.bytes.is_empty() || self.bytes.ends_with(b"\n")
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Text({} lines, {} bytes)",
            self.line_count(),
            self.bytes.len()
        )
    }
}

/// The differences between a base tree and a target, one entry per changed
/// file. Immutable and shared as `Arc<ChangeSet>`; `Send + Sync`.
pub struct ChangeSet {
    base_tree: Oid,
    target: Target,
    files: Vec<Change>,
    totals: Totals,
    limits: Limits,
    /// Per file, the new side's bytes when they came from the worktree and
    /// were kept (within `Limits::retained_bytes`). Never set for trees.
    retained: Vec<Option<Arc<[u8]>>>,
}

impl fmt::Debug for ChangeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChangeSet")
            .field("base_tree", &self.base_tree)
            .field("target", &self.target)
            .field("files", &self.files)
            .field("totals", &self.totals)
            .finish_non_exhaustive()
    }
}

impl ChangeSet {
    pub fn base_tree(&self) -> Oid {
        self.base_tree
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Sorted by (path bytes, `untracked`), truncated files included.
    pub fn files(&self) -> &[Change] {
        &self.files
    }

    pub fn totals(&self) -> &Totals {
        &self.totals
    }

    /// The texts of `files()[file]`. Object-database sides are read by OID
    /// (up to `Limits::diff_bytes`); a worktree side is the content read
    /// during the comparison, never reread by path.
    pub fn texts(&self, repo: &Repo, file: usize) -> Result<FileTexts, GitError> {
        let Some(change) = self.files.get(file) else {
            return Err(GitError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no file {file} in a change set of {}", self.files.len()),
            )));
        };
        let handle = repo.open_common()?;
        let side = |side: Option<&Side>,
                    retained: Option<&Arc<[u8]>>|
         -> Result<SideText, GitError> {
            let Some(side) = side else {
                return Ok(SideText::Absent);
            };
            if side.mode == Mode::Gitlink || side.mode == Mode::Special {
                return Ok(SideText::Unavailable);
            }
            match &side.content {
                Content::Unavailable(Unavailable::NotRetained { .. }) => Ok(SideText::NotRetained),
                Content::Unavailable(_) | Content::Unhashed { .. } => Ok(SideText::Unavailable),
                Content::Blob(oid) => {
                    if let Some(bytes) = retained {
                        return Ok(SideText::Text(Text::new(Arc::clone(bytes))));
                    }
                    match read_blob(&handle, *oid, self.limits.diff_bytes)? {
                        Blob::Bytes(bytes) => Ok(SideText::Text(Text::new(bytes.into()))),
                        Blob::TooLarge(size) => Ok(SideText::TooLarge { size }),
                        Blob::Missing => Ok(SideText::Unavailable),
                    }
                }
            }
        };
        Ok(FileTexts {
            old: side(change.old.as_ref(), None)?,
            new: side(
                change.new.as_ref(),
                self.retained.get(file).and_then(Option::as_ref),
            )?,
        })
    }
}

enum Blob {
    Bytes(Vec<u8>),
    TooLarge(u64),
    Missing,
}

fn read_blob(handle: &Handle, oid: Oid, max: u64) -> Result<Blob, GitError> {
    let Some(header) = handle
        .try_find_header(oid.to_gix())
        .map_err(GitError::from_gix)?
    else {
        return Ok(Blob::Missing);
    };
    if header.size() > max {
        return Ok(Blob::TooLarge(header.size()));
    }
    match handle
        .try_find_object(oid.to_gix())
        .map_err(GitError::from_gix)?
    {
        Some(object) => Ok(Blob::Bytes(object.detach().data)),
        None => Ok(Blob::Missing),
    }
}

/// Peels a commit (or a tag) to its tree; a tree is returned as is. The
/// empty tree needs no object (gix knows it).
fn peel_to_tree(handle: &Handle, id: Oid) -> Result<Oid, GitError> {
    let object = handle
        .try_find_object(id.to_gix())
        .map_err(GitError::from_gix)?
        .ok_or(GitError::ObjectMissing(id))?;
    let tree = object
        .peel_to_kind(gix::objs::Kind::Tree)
        .map_err(|err| missing_or(err, id))?;
    Ok(Oid::from_gix(&tree.id))
}

impl Repo {
    /// The change set between two trees, or two commits' trees (a commit
    /// against its first parent, a branch against the merge base; a root
    /// commit against [`Oid::empty_tree`]). Renames are detected as
    /// `git diff -M` does, with this repository's `diff.renameLimit`.
    pub fn diff_trees(
        &self,
        old: Oid,
        new: Oid,
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Result<Arc<ChangeSet>, GitError> {
        let handle = self.open_common()?;
        let old_tree = peel_to_tree(&handle, old)?;
        let new_tree = peel_to_tree(&handle, new)?;
        let walk = tree_diff::diff(&handle, old_tree, new_tree, limits.entries, cancel)?;
        let mut attributes = TreeAttributes::new(&handle, self.hash(), &walk.attributes)?;
        let mut invalid = walk.invalid;

        let mut pending = Vec::new();
        let mut deleted: Vec<(RepoPath, Mode, Oid)> = Vec::new();
        let mut added: Vec<(RepoPath, Mode, Oid)> = Vec::new();
        for entry in walk.entries {
            let Some(path) = RepoPath::new(&entry.path) else {
                invalid.push(entry.path.into());
                continue;
            };
            match (entry.old, entry.new) {
                (Some(old), Some(new)) => {
                    let status = if kind_of(old.0) == kind_of(new.0) {
                        Status::Modified
                    } else {
                        Status::TypeChanged
                    };
                    pending.push(Pending::new(
                        status,
                        path,
                        Some(tree_side(old)),
                        Some(tree_side(new)),
                    ));
                }
                (Some((mode, oid)), None) => deleted.push((path, mode, oid)),
                (None, Some((mode, oid))) => added.push((path, mode, oid)),
                (None, None) => {}
            }
        }

        // git: `diff.renameLimit` 0 or negative is unlimited.
        let limit = handle
            .config_snapshot()
            .integer("diff.renameLimit")
            .map_or(Some(DEFAULT_RENAME_LIMIT), |l| u64::try_from(l).ok());
        fn candidates(files: &[(RepoPath, Mode, Oid)]) -> Vec<Candidate<'_>> {
            files
                .iter()
                .map(|(path, mode, oid)| Candidate {
                    path: path.as_bytes(),
                    mode: *mode,
                    oid: *oid,
                })
                .collect()
        }
        let pairs = rename::detect(
            &candidates(&deleted),
            &candidates(&added),
            limit,
            &mut |is_source: bool, i: usize| -> Result<Option<Signature>, GitError> {
                let (path, _, oid) = if is_source { &deleted[i] } else { &added[i] };
                let binary = attributes.get(path)?.binary;
                Ok(match read_blob(&handle, *oid, limits.diff_bytes)? {
                    Blob::Bytes(bytes) => Some(Signature::new(&bytes, !binary && !has_nul(&bytes))),
                    Blob::TooLarge(_) | Blob::Missing => None,
                })
            },
            cancel,
        )?;

        let mut source_used = vec![false; deleted.len()];
        let mut dest_used = vec![false; added.len()];
        for pair in &pairs {
            source_used[pair.source] = true;
            dest_used[pair.dest] = true;
            let (from, old_mode, old_oid) = &deleted[pair.source];
            let (path, new_mode, new_oid) = &added[pair.dest];
            pending.push(Pending::new(
                Status::Renamed {
                    from: from.clone(),
                    similarity: pair.similarity(),
                },
                path.clone(),
                Some(tree_side((*old_mode, *old_oid))),
                Some(tree_side((*new_mode, *new_oid))),
            ));
        }
        for (i, (path, mode, oid)) in deleted.into_iter().enumerate() {
            if !source_used[i] {
                pending.push(Pending::new(
                    Status::Deleted,
                    path,
                    Some(tree_side((mode, oid))),
                    None,
                ));
            }
        }
        for (i, (path, mode, oid)) in added.into_iter().enumerate() {
            if !dest_used[i] {
                pending.push(Pending::new(
                    Status::Added,
                    path,
                    None,
                    Some(tree_side((mode, oid))),
                ));
            }
        }
        pending.sort_by(|a, b| a.path.cmp(&b.path).then(a.untracked.cmp(&b.untracked)));

        let mut attrs = |path: &RepoPath| attributes.get(path);
        let assembled = Assembler {
            handle: &handle,
            limits,
            cancel,
            attrs: &mut attrs,
        }
        .run(pending)?;
        invalid.sort_unstable();
        Ok(Arc::new(ChangeSet {
            base_tree: old_tree,
            target: Target::Tree(new_tree),
            files: assembled.files,
            totals: Totals {
                invalid_paths: invalid.into(),
                ..assembled.totals
            },
            limits: limits.clone(),
            retained: assembled.retained,
        }))
    }
}

/// git's default `diff.renameLimit`.
const DEFAULT_RENAME_LIMIT: u64 = 1000;

/// Regular files, symbolic links, submodules and special entries are
/// different kinds: a change between them is `TypeChanged`.
fn kind_of(mode: Mode) -> u8 {
    match mode {
        Mode::File | Mode::Executable => 0,
        Mode::Symlink => 1,
        Mode::Gitlink => 2,
        Mode::Special => 3,
    }
}

fn tree_side((mode, oid): (Mode, Oid)) -> PendingSide {
    PendingSide {
        mode,
        content: Content::Blob(oid),
        bytes: None,
    }
}

/// git's binary check: a NUL in the first 8000 bytes.
fn has_nul(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0)
}

/// One change before its `Detail` is computed.
pub(crate) struct Pending {
    pub status: Status,
    pub path: RepoPath,
    pub old: Option<PendingSide>,
    pub new: Option<PendingSide>,
    pub untracked: bool,
    pub staged: Option<Staged>,
}

impl Pending {
    pub(crate) fn new(
        status: Status,
        path: RepoPath,
        old: Option<PendingSide>,
        new: Option<PendingSide>,
    ) -> Pending {
        Pending {
            status,
            path,
            old,
            new,
            untracked: false,
            staged: None,
        }
    }
}

pub(crate) struct PendingSide {
    pub mode: Mode,
    pub content: Content,
    /// The content's bytes when they came from the worktree rather than
    /// the object database.
    pub bytes: Option<Arc<[u8]>>,
}

pub(crate) struct Assembled {
    pub files: Vec<Change>,
    pub totals: Totals,
    pub retained: Vec<Option<Arc<[u8]>>>,
}

/// Computes each file's `Detail` in path order under the limits, and decides
/// which worktree contents the change set keeps.
pub(crate) struct Assembler<'a> {
    pub handle: &'a Handle,
    pub limits: &'a Limits,
    pub cancel: &'a AtomicBool,
    pub attrs: &'a mut dyn FnMut(&RepoPath) -> Result<Attrs, GitError>,
}

/// A side's bytes, from memory or the object database.
enum Loaded {
    Bytes(Arc<[u8]>),
    Object(Vec<u8>),
}

impl Loaded {
    fn bytes(&self) -> &[u8] {
        match self {
            Loaded::Bytes(b) => b,
            Loaded::Object(b) => b,
        }
    }
}

/// A rough size of the patch's file header (`diff --git`, `index`, `---`,
/// `+++`): counted towards the total patch limit.
fn header_bytes(old_path: &RepoPath, new_path: &RepoPath) -> u64 {
    (2 * (old_path.as_bytes().len() + new_path.as_bytes().len()) + 100) as u64
}

impl Assembler<'_> {
    pub(crate) fn run(mut self, pending: Vec<Pending>) -> Result<Assembled, GitError> {
        let mut files = Vec::with_capacity(pending.len());
        let mut retained = Vec::with_capacity(pending.len());
        let mut totals = Totals::default();
        let mut patch_total = 0u64;
        let mut kept = 0u64;
        let mut keeping = true;
        for mut p in pending {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(GitError::Cancelled);
            }
            let over_total = files.len() >= self.limits.total_files
                || patch_total >= self.limits.total_patch_bytes;
            let (mut detail, patch) = if over_total {
                (Detail::Listed, 0)
            } else {
                self.detail(&mut p)?
            };
            patch_total += patch;
            let old_path = match &p.status {
                Status::Renamed { from, .. } => from.clone(),
                _ => p.path.clone(),
            };
            let attrs_path = if p.new.is_some() { &p.path } else { &old_path };
            let generated = (self.attrs)(attrs_path)?.generated;

            // Worktree bytes are kept in path order until the limit; past it
            // (and for every later file) the new side is `NotRetained`. Bytes
            // equal to the old side's blob need no keeping: they are in the
            // object database.
            let mut keep = None;
            if let Some(new) = &mut p.new
                && let Some(bytes) = new.bytes.take()
                && let Content::Blob(oid) = new.content
            {
                let in_odb = p.old.as_ref().and_then(|o| o.content.oid()) == Some(oid);
                if !in_odb {
                    if keeping && kept + bytes.len() as u64 <= self.limits.retained_bytes {
                        kept += bytes.len() as u64;
                        keep = Some(bytes);
                    } else {
                        keeping = false;
                        new.content = Content::Unavailable(Unavailable::NotRetained { oid });
                        totals.not_retained += 1;
                        if let Detail::Text(diff) = &detail {
                            detail = Detail::Truncated {
                                added: diff.added,
                                removed: diff.removed,
                            };
                        }
                    }
                }
            }

            match &detail {
                Detail::Text(diff) => {
                    totals.added += u64::from(diff.added);
                    totals.removed += u64::from(diff.removed);
                }
                Detail::Truncated { added, removed } => {
                    totals.added += u64::from(*added);
                    totals.removed += u64::from(*removed);
                    totals.truncated += 1;
                }
                Detail::TooLarge => totals.too_large += 1,
                Detail::Listed => totals.listed += 1,
                _ => {}
            }
            let side = |s: Option<PendingSide>| {
                s.map(|s| Side {
                    mode: s.mode,
                    content: s.content,
                })
            };
            files.push(Change {
                status: p.status,
                path: p.path,
                old: side(p.old),
                new: side(p.new),
                detail,
                generated,
                untracked: p.untracked,
                staged: p.staged,
            });
            retained.push(keep);
        }
        totals.files = files.len();
        Ok(Assembled {
            files,
            totals,
            retained,
        })
    }

    /// One file's `Detail` and the patch bytes it adds to the total. May mark
    /// a side whose object is missing as `Unavailable::Missing`.
    fn detail(&mut self, p: &mut Pending) -> Result<(Detail, u64), GitError> {
        let old_path = match &p.status {
            Status::Renamed { from, .. } => from.clone(),
            _ => p.path.clone(),
        };
        let sides = [p.old.as_ref(), p.new.as_ref()];
        let no_diff = sides.iter().flatten().any(|s| {
            s.mode == Mode::Gitlink
                || s.mode == Mode::Special
                || !matches!(s.content, Content::Blob(_))
        });
        if no_diff {
            return Ok((Detail::NoDiff, 0));
        }
        let oid = |s: Option<&PendingSide>| s.and_then(|s| s.content.oid());
        let (old_oid, new_oid) = (oid(p.old.as_ref()), oid(p.new.as_ref()));
        let type_changed = p.status == Status::TypeChanged;
        if !type_changed && old_oid.is_some() && old_oid == new_oid {
            let same_mode = p.old.as_ref().map(|s| s.mode) == p.new.as_ref().map(|s| s.mode);
            return Ok(if same_mode {
                (Detail::Text(FileDiff::empty()), 0)
            } else {
                (Detail::ModeOnly, 0)
            });
        }

        // Sizes first: a missing object makes the side unavailable.
        let mut sizes = [None, None];
        for (i, side) in [&mut p.old, &mut p.new].into_iter().enumerate() {
            let Some(side) = side else { continue };
            match self.size(side)? {
                Some(size) => sizes[i] = Some(size),
                None => {
                    side.content = Content::Unavailable(Unavailable::Missing);
                    return Ok((Detail::NoDiff, 0));
                }
            }
        }
        let binary = Detail::Binary {
            old_size: sizes[0],
            new_size: sizes[1],
        };
        let mut attr_binary = (self.attrs)(&p.path)?.binary;
        if p.old.is_some() && old_path != p.path {
            attr_binary |= (self.attrs)(&old_path)?.binary;
        }
        if attr_binary {
            return Ok((binary, 0));
        }
        if sizes.iter().flatten().any(|&s| s > self.limits.diff_bytes) {
            return Ok((Detail::TooLarge, 0));
        }

        let both = p.old.is_some() && p.new.is_some() && !type_changed;
        let key = match (old_oid, new_oid) {
            (Some(o), Some(n)) if both => Some(HunkKey::lines(o, n)),
            _ => None,
        };
        let computed = match key.as_ref().and_then(hunks::cached) {
            Some(hit) => hit,
            None => {
                let old = p.old.as_ref().map(|s| self.load(s)).transpose()?.flatten();
                let new = p.new.as_ref().map(|s| self.load(s)).transpose()?.flatten();
                let (old, new) = match (p.old.is_some(), old, p.new.is_some(), new) {
                    (true, None, ..) | (_, _, true, None) => return Ok((Detail::NoDiff, 0)),
                    (_, old, _, new) => (old, new),
                };
                let old = old.as_ref().map_or(&b""[..], Loaded::bytes);
                let new = new.as_ref().map_or(&b""[..], Loaded::bytes);
                if has_nul(old) || has_nul(new) {
                    return Ok((binary, 0));
                }
                let computed = if both {
                    hunks::line_diff(old, new)
                } else {
                    hunks::whole(old, new)
                };
                if let Some(key) = key {
                    hunks::remember(key, computed.clone());
                }
                computed
            }
        };
        let Computed { diff, body_bytes } = computed;
        let patch = body_bytes + header_bytes(&old_path, &p.path);
        let lines = u64::from(diff.added) + u64::from(diff.removed);
        if lines > u64::from(self.limits.patch_lines) || body_bytes > self.limits.patch_bytes {
            // Only the header and the counts are generated.
            return Ok((
                Detail::Truncated {
                    added: diff.added,
                    removed: diff.removed,
                },
                header_bytes(&old_path, &p.path),
            ));
        }
        Ok((Detail::Text(diff), patch))
    }

    /// The side's size; None when its object is missing.
    fn size(&self, side: &PendingSide) -> Result<Option<u64>, GitError> {
        if let Some(bytes) = &side.bytes {
            return Ok(Some(bytes.len() as u64));
        }
        let Some(oid) = side.content.oid() else {
            return Ok(None);
        };
        Ok(self
            .handle
            .try_find_header(oid.to_gix())
            .map_err(GitError::from_gix)?
            .map(|h| h.size()))
    }

    /// The side's bytes; None when its object is missing.
    fn load(&self, side: &PendingSide) -> Result<Option<Loaded>, GitError> {
        if let Some(bytes) = &side.bytes {
            return Ok(Some(Loaded::Bytes(Arc::clone(bytes))));
        }
        let Some(oid) = side.content.oid() else {
            return Ok(None);
        };
        Ok(self
            .handle
            .try_find_object(oid.to_gix())
            .map_err(GitError::from_gix)?
            .map(|o| Loaded::Object(o.detach().data)))
    }
}

#[cfg(test)]
mod tests;
