//! The attributes a diff needs: `binary` / `-diff` and `linguist-generated`
//! (docs/design/git.md「上限与截断」, 「数据结构」). For a tree diff they come
//! from the new tree's `.gitattributes` files, plus `info/attributes` and the
//! global and system attribute files the handle's permissions allow; the
//! worktree is never read.

use gix::bstr::ByteSlice;

use crate::handle::Handle;
use crate::{GitError, HashKind, Oid, RepoPath};

/// What a path's attributes say about diffing it.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) struct Attrs {
    /// `-diff` (also through the `binary` macro): never a text diff.
    pub binary: bool,
    /// `linguist-generated` (set, or `=true`).
    pub generated: bool,
}

/// Attribute lookups against a set of in-tree `.gitattributes` blobs.
pub(crate) struct TreeAttributes<'h> {
    stack: gix::AttributeStack<'h>,
    outcome: gix::attrs::search::Outcome,
}

impl<'h> TreeAttributes<'h> {
    /// `files` are the tree's `.gitattributes` blobs that can apply: full
    /// path (`dir/.gitattributes`) and OID. Only these are mapped, so the
    /// stack reads them from the object database and nothing else in-tree.
    pub(crate) fn new(
        handle: &'h Handle,
        hash: HashKind,
        files: &[(Vec<u8>, Oid)],
    ) -> Result<TreeAttributes<'h>, GitError> {
        use gix::index::entry::{Flags, Mode, Stat};

        let mut state = gix::index::State::new(hash.to_gix());
        for (path, oid) in files {
            state.dangerously_push_entry(
                Stat::default(),
                oid.to_gix(),
                Flags::empty(),
                Mode::FILE,
                path.as_bstr(),
            );
        }
        state.sort_entries();
        let stack = handle
            .attributes_only(
                &state,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )
            .map_err(GitError::from_gix)?;
        let outcome = stack.selected_attribute_matches(["diff", "linguist-generated"]);
        Ok(TreeAttributes { stack, outcome })
    }

    pub(crate) fn get(&mut self, path: &RepoPath) -> Result<Attrs, GitError> {
        use gix::attrs::StateRef;

        let platform = self.stack.at_entry(
            path.as_bytes().as_bstr(),
            Some(gix::index::entry::Mode::FILE),
        )?;
        platform.matching_attributes(&mut self.outcome);
        let mut attrs = Attrs::default();
        for m in self.outcome.iter_selected() {
            match (m.assignment.name.as_str(), m.assignment.state) {
                ("diff", StateRef::Unset) => attrs.binary = true,
                ("linguist-generated", StateRef::Set) => attrs.generated = true,
                ("linguist-generated", StateRef::Value(v)) => {
                    attrs.generated = v.as_bstr().eq_ignore_ascii_case(b"true");
                }
                _ => {}
            }
        }
        Ok(attrs)
    }
}
