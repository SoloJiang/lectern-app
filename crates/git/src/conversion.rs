//! Worktree attributes and git's built-in conversions to the canonical form
//! (docs/design/git.md「工作区比较」第 1、3 步, ADR 0024). The `.gitattributes`
//! files in effect are read by the caller through `SafeRoot` (or taken from
//! the index) and given here as contents; the attribute stack finds them by
//! OID in an in-memory overlay, so nothing in the worktree is read by path
//! and nothing is written to the object database. The filter pipeline has no
//! drivers: no clean / process filter can run, and a path whose `filter`
//! attribute names a configured driver is reported as filtered instead.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use gix::bstr::{BStr, ByteSlice};
use gix::filter::plumbing::pipeline::CrlfRoundTripCheck;

use crate::attrs::Attrs;
use crate::handle::Handle;
use crate::{GitError, HashKind, Oid, RepoPath, gitdir};

/// One `.gitattributes` file in effect: its path and content. `content` is
/// None when it comes from the object database (the index's blob for a
/// file missing from the worktree, or a skip-worktree entry).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct AttrFile {
    pub path: Vec<u8>,
    pub oid: Oid,
    pub content: Option<Arc<[u8]>>,
}

/// What a canonical content depends on: the attribute files in effect, and
/// the global attributes and configuration (docs/design/git.md「工作区比较」,
/// stat 缓存). Kept by a `Worktree` as the baseline its stat comparisons are
/// checked against, and by a change set for `refresh(Paths)`.
#[derive(Debug)]
pub(crate) struct ConversionState {
    pub fingerprint: u64,
    pub files: Arc<[AttrFile]>,
    /// `info/attributes` and the conversion configuration.
    pub globals: u64,
}

impl ConversionState {
    pub(crate) fn new(
        handle: &Handle,
        mut files: Vec<AttrFile>,
    ) -> Result<ConversionState, GitError> {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        let mut h = DefaultHasher::new();
        let info = gitdir::read(&handle.common_dir().join("info").join("attributes"))?;
        info.hash(&mut h);
        config_digest(handle).hash(&mut h);
        let globals = h.finish();
        let mut h = DefaultHasher::new();
        globals.hash(&mut h);
        for f in &files {
            f.path.hash(&mut h);
            f.oid.hash(&mut h);
        }
        Ok(ConversionState {
            fingerprint: h.finish(),
            files: files.into(),
            globals,
        })
    }
}

/// The configuration that changes canonical contents or modes, as text.
fn config_digest(handle: &Handle) -> Vec<u8> {
    let config = handle.config_snapshot();
    let mut out = Vec::new();
    for key in [
        "core.autocrlf",
        "core.eol",
        "core.safecrlf",
        "core.attributesFile",
        "core.fileMode",
        "core.symlinks",
    ] {
        out.extend_from_slice(key.as_bytes());
        out.push(b'=');
        if let Some(value) = config.string(key) {
            out.extend_from_slice(&value);
        }
        out.push(0);
    }
    if let Some(sections) = config.plumbing().sections_by_name("filter") {
        for section in sections {
            out.extend_from_slice(&section.to_bstring());
            out.push(0);
        }
    }
    out
}

/// The names of filter drivers with a `clean` or `process` command: a path
/// whose `filter` attribute names one can't be compared without running it.
fn configured_filters(handle: &Handle) -> HashSet<Vec<u8>> {
    let config = handle.config_snapshot();
    let mut out = HashSet::new();
    if let Some(sections) = config.plumbing().sections_by_name("filter") {
        for section in sections {
            let Some(name) = section.header().subsection_name() else {
                continue;
            };
            if section.value("clean").is_some() || section.value("process").is_some() {
                out.insert(name.to_vec());
            }
        }
    }
    out
}

/// What a worktree path's attributes say: the diff attributes, whether a
/// configured filter applies, and a digest of every rule that changes its
/// canonical content.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) struct PathAttrs {
    pub diff: Attrs,
    pub filtered: bool,
    pub rule: u64,
}

const SELECTED: [&str; 8] = [
    "diff",
    "linguist-generated",
    "crlf",
    "ident",
    "filter",
    "eol",
    "text",
    "working-tree-encoding",
];

/// Attribute lookups and to-git conversions for one worktree comparison.
pub(crate) struct WorktreeAttrs<'h> {
    handle: &'h Handle,
    stack: gix::AttributeStack<'h>,
    blobs: HashMap<gix::ObjectId, Arc<[u8]>>,
    outcome: gix::attrs::search::Outcome,
    pipeline: gix::filter::plumbing::Pipeline,
    filters: HashSet<Vec<u8>>,
    cache: HashMap<RepoPath, PathAttrs>,
}

/// The object database plus the worktree's attribute files, for the stack's
/// id mappings. Only blobs are served from memory.
struct Overlay<'a> {
    odb: &'a gix::OdbHandle,
    blobs: &'a HashMap<gix::ObjectId, Arc<[u8]>>,
}

impl gix::objs::Find for Overlay<'_> {
    fn try_find<'b>(
        &self,
        id: &gix::hash::oid,
        buffer: &'b mut Vec<u8>,
    ) -> gix::ExnResult<Option<gix::objs::Data<'b>>> {
        if let Some(bytes) = self.blobs.get(id) {
            buffer.clear();
            buffer.extend_from_slice(bytes);
            return Ok(Some(gix::objs::Data::new(
                buffer,
                gix::objs::Kind::Blob,
                id.kind(),
            )));
        }
        gix::objs::Find::try_find(self.odb, id, buffer)
    }
}

impl<'h> WorktreeAttrs<'h> {
    pub(crate) fn new(
        handle: &'h Handle,
        hash: HashKind,
        files: &[AttrFile],
    ) -> Result<WorktreeAttrs<'h>, GitError> {
        use gix::index::entry::{Flags, Mode, Stat};

        let mut state = gix::index::State::new(hash.to_gix());
        let mut blobs = HashMap::new();
        for file in files {
            state.dangerously_push_entry(
                Stat::default(),
                file.oid.to_gix(),
                Flags::empty(),
                Mode::FILE,
                file.path.as_bstr(),
            );
            if let Some(content) = &file.content {
                blobs.insert(file.oid.to_gix(), Arc::clone(content));
            }
        }
        state.sort_entries();
        let stack = handle
            .attributes_only(
                &state,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )
            .map_err(GitError::from_gix)?;
        let outcome = stack.selected_attribute_matches(SELECTED);
        // The drivers stay empty (ADR 0024): `filter=` never runs anything.
        let mut options = gix::filter::Pipeline::options(handle).map_err(GitError::from_gix)?;
        options.drivers = Vec::new();
        options.crlf_roundtrip_check = CrlfRoundTripCheck::Skip;
        options.encodings_with_roundtrip_check = Vec::new();
        let pipeline = gix::filter::plumbing::Pipeline::new(Default::default(), options);
        Ok(WorktreeAttrs {
            handle,
            stack,
            blobs,
            outcome,
            pipeline,
            filters: configured_filters(handle),
            cache: HashMap::new(),
        })
    }

    fn platform(
        &mut self,
        path: &RepoPath,
    ) -> Result<gix::worktree::stack::Platform<'_>, GitError> {
        let overlay = Overlay {
            odb: &self.handle.objects,
            blobs: &self.blobs,
        };
        let stack: &mut gix::worktree::Stack = &mut self.stack;
        Ok(stack.at_entry(
            path.as_bytes().as_bstr(),
            Some(gix::index::entry::Mode::FILE),
            &overlay,
        )?)
    }

    pub(crate) fn get(&mut self, path: &RepoPath) -> Result<PathAttrs, GitError> {
        use gix::attrs::StateRef;

        if let Some(attrs) = self.cache.get(path) {
            return Ok(*attrs);
        }
        let mut outcome = std::mem::take(&mut self.outcome);
        self.platform(path)?.matching_attributes(&mut outcome);
        let mut attrs = PathAttrs::default();
        let mut rule = DefaultHasher::new();
        for (i, m) in outcome.iter_selected().enumerate() {
            let state = m.assignment.state;
            match (i, state) {
                (0, StateRef::Unset) => attrs.diff.binary = true,
                (1, StateRef::Set) => attrs.diff.generated = true,
                (1, StateRef::Value(v)) => {
                    attrs.diff.generated = v.as_bstr().eq_ignore_ascii_case(b"true");
                }
                (4, StateRef::Value(v)) => {
                    attrs.filtered = self.filters.contains(v.as_bstr().as_bytes());
                }
                _ => {}
            }
            if i >= 2 {
                match state {
                    StateRef::Set => 1u8.hash(&mut rule),
                    StateRef::Unset => 2u8.hash(&mut rule),
                    StateRef::Value(v) => {
                        3u8.hash(&mut rule);
                        v.as_bstr().as_bytes().hash(&mut rule);
                    }
                    StateRef::Unspecified => 0u8.hash(&mut rule),
                }
            }
        }
        attrs.filtered.hash(&mut rule);
        attrs.rule = rule.finish();
        self.outcome = outcome;
        self.cache.insert(path.clone(), attrs);
        Ok(attrs)
    }

    /// The canonical content of `raw` at `path`: `working-tree-encoding`,
    /// end-of-line conversion and `ident`, as `git add` would store it.
    /// `index_blob` is the index's blob at the path, which the `text=auto`
    /// conversion checks for CRs (git's `has_crlf_in_index`). A conversion
    /// that fails (e.g. bytes that aren't valid in the declared encoding)
    /// leaves the content raw, as git's diff does.
    pub(crate) fn canonical(
        &mut self,
        path: &RepoPath,
        raw: Vec<u8>,
        index_blob: Option<Oid>,
    ) -> Result<Vec<u8>, GitError> {
        use gix::filter::plumbing::pipeline::convert::ToGitOutcome;

        let rela = os_path(path.as_bytes());
        let overlay = Overlay {
            odb: &self.handle.objects,
            blobs: &self.blobs,
        };
        let stack: &mut gix::worktree::Stack = &mut self.stack;
        let platform = stack.at_entry(
            path.as_bytes().as_bstr(),
            Some(gix::index::entry::Mode::FILE),
            &overlay,
        )?;
        let odb = &self.handle.objects;
        let converted = self.pipeline.convert_to_git(
            &raw[..],
            &rela,
            &mut |_: &BStr, out: &mut gix::attrs::search::Outcome| {
                platform.matching_attributes(out);
            },
            &mut |buf: &mut Vec<u8>| -> gix::ExnResult<Option<()>> {
                let Some(id) = index_blob else {
                    return Ok(None);
                };
                let found = gix::objs::Find::try_find(odb, &id.to_gix(), buf)?;
                Ok(found
                    .filter(|o| o.kind == gix::objs::Kind::Blob)
                    .map(|_| ()))
            },
        );
        Ok(match converted {
            Ok(ToGitOutcome::Buffer(bytes)) => bytes.to_vec(),
            Ok(ToGitOutcome::Unchanged(_)) | Ok(ToGitOutcome::Process(_)) | Err(_) => raw,
        })
    }
}

/// Repository-relative path bytes as a `Path`, for gix calls that take one.
#[cfg(unix)]
pub(crate) fn os_path(bytes: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
pub(crate) fn os_path(bytes: &[u8]) -> std::path::PathBuf {
    std::path::PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}
