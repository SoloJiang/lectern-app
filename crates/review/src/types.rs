//! Stand-ins for the types of prerequisite A (the `changes` and `review`
//! modules of `lectern-services`, docs/design/review.md「接口」) until it is
//! merged.
//!
//! Names, variants and fields are the design's, limited to what step 1 uses
//! (the noise list, rule grouping, the annotation log and seen state). Step 3
//! deletes this module and uses the `lectern_services` types instead; the
//! annotation log's format is the log's own encoding (`log::wire`), so the
//! swap does not change it.

pub mod changes {
    use std::sync::Arc;

    use lectern_services::project::BlobId;

    /// Relative to the worktree root, `/`-separated raw bytes (git allows
    /// names that are not UTF-8).
    #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
    pub struct ChangePath(pub Arc<[u8]>);

    impl ChangePath {
        /// Some when the path is UTF-8.
        pub fn as_str(&self) -> Option<&str> {
            std::str::from_utf8(&self.0).ok()
        }
    }

    /// One changed file. Only the fields step 1 reads; `detail`,
    /// `untracked` and `staged` come with prerequisite A.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct FileChange {
        pub status: ChangeStatus,
        /// The new path; the old one for a deletion.
        pub path: ChangePath,
        pub old: Option<FileSide>,
        pub new: Option<FileSide>,
        /// `.gitattributes`' `linguist-generated`.
        pub generated: bool,
    }

    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub enum ChangeStatus {
        Added,
        Deleted,
        Modified,
        TypeChanged,
        Renamed { from: ChangePath, similarity: u8 },
        Conflicted,
    }

    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub struct FileSide {
        pub mode: FileMode,
        pub content: SideIdentity,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub enum FileMode {
        File,
        Executable,
        Symlink,
        Gitlink,
        Special,
    }

    /// The identity of one side's content.
    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub enum SideIdentity {
        Blob(BlobId),
        /// Hashed during the comparison, but the bytes were over the
        /// retention limit and not kept.
        NotRetained(BlobId),
        /// Over the worktree's single-file read limit; not read.
        Unhashed {
            size: u64,
            mtime_ns: i64,
            inode: u64,
        },
        /// A clean / process filter applies.
        Filtered(RawIdentity),
        Unavailable(UnavailableKind),
    }

    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub enum RawIdentity {
        Sha256([u8; 32]),
        Stat {
            size: u64,
            mtime_ns: i64,
            inode: u64,
        },
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub enum UnavailableKind {
        UnsupportedType,
        PathThroughSymlink,
        NestedRepository,
        Missing,
        Unreadable,
    }

    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub enum BaseSpec {
        Head,
        /// A commit's tree, such as HEAD when a session started.
        Commit(BlobId),
        /// The tree of `merge_base(HEAD, branch)`; a full ref name.
        MergeBase(Arc<str>),
    }
}

pub mod review {
    use std::sync::Arc;

    use lectern_services::session::Timestamp;

    use super::changes::ChangePath;

    /// Declared in display order (「噪声文件与规则分组」).
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub enum GroupKind {
        Code,
        Tests,
        Config,
        Deps,
        Docs,
        Noise(NoiseKind),
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub enum NoiseKind {
        Lockfile,
        Generated,
        Snapshot,
    }

    /// In `GroupKind` declaration order; within a group by subproject, then
    /// path.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct Group {
        pub kind: GroupKind,
        /// Indices into the snapshot's `files`.
        pub files: Arc<[u32]>,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub enum SensitiveKind {
        AgentInstructions,
        Ci,
        Hooks,
        GitMetadata,
        Symlink,
        Executable,
        Submodule,
    }

    /// A file's stable identity in a snapshot. Added (None, new path);
    /// deleted (old path, None); modified, type change, conflict (path,
    /// path); renamed (source, target).
    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub struct FileKey {
        pub old: Option<ChangePath>,
        pub new: Option<ChangePath>,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum SeenState {
        Unseen,
        Seen,
        /// Changed since seen: this review target has a seen mark for the
        /// same change, with different values in its key. `since` is when
        /// that mark was made.
        ChangedSinceSeen {
            since: Timestamp,
        },
    }

    #[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
    pub struct Progress {
        pub total: u32,
        pub seen: u32,
        pub changed_since_seen: u32,
    }
}
