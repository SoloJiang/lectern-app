//! Opening gix handles (docs/design/git.md「数据流与线程」, ADR 0024).
//!
//! A [`Handle`] lives for one top-level call on the calling thread and is
//! dropped before the call returns; `Repo` and `Worktree` never hold one.
//! With `parallel` off the gix handle isn't `Send`, which the compiler
//! enforces; the counting hook in tests checks that none outlives a call.

use std::ops::Deref;
use std::path::Path;

use gix::sec::Permission;

use crate::GitError;
use crate::error::gix_err;

/// Which configuration a handle reads besides the repository's own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConfigScope {
    /// System (`/etc/gitconfig`), XDG and `~/.gitconfig`, as git reads them.
    /// No `GIT_*` environment variable takes effect and the git binary is
    /// never run to locate its installation config.
    User,
    /// Only the repository's own configuration: tests, so that the machine's
    /// configuration can't change their results.
    RepositoryOnly,
}

/// Each handle's delta-base cache; the object cache stays off (gix leaves it
/// off without `max-performance-safe`). Budget: ≤ 4 MiB per handle.
const PACK_CACHE_BYTES: usize = 4 * 1024 * 1024;

fn options(scope: ConfigScope) -> gix::open::Options {
    let permissions = match scope {
        ConfigScope::User => {
            let deny = Permission::Deny;
            gix::open::Permissions {
                env: gix::open::permissions::Environment {
                    // Only to locate ~/.gitconfig and $XDG_CONFIG_HOME/git/config.
                    xdg_config_home: Permission::Allow,
                    home: Permission::Allow,
                    http_transport: deny,
                    identity: deny,
                    objects: deny,
                    git_prefix: deny,
                    other: deny,
                    ssh_prefix: deny,
                },
                config: gix::open::permissions::Config {
                    git_binary: false,
                    system: true,
                    git: true,
                    user: true,
                    env: false,
                    includes: true,
                },
                attributes: gix::open::permissions::Attributes {
                    git_binary: false,
                    system: true,
                    git: true,
                },
            }
        }
        ConfigScope::RepositoryOnly => gix::open::Permissions::isolated(),
    };
    gix::open::Options::default()
        .permissions(permissions)
        .open_path_as_is(true)
}

/// A gix repository handle for the duration of one call.
pub(crate) struct Handle {
    repo: gix::Repository,
}

impl Handle {
    /// Opens the git directory `git_dir` itself (a common directory, a
    /// linked worktree's administrative directory or a bare repository).
    pub(crate) fn open(git_dir: &Path, scope: ConfigScope) -> Result<Handle, GitError> {
        let mut repo = gix::open_opts(git_dir, options(scope)).map_err(gix_err)?;
        repo.objects.set_pack_cache(|| {
            Box::new(gix::odb::pack::cache::lru::MemoryCappedHashmap::new(
                PACK_CACHE_BYTES,
            ))
        });
        hook::opened();
        Ok(Handle { repo })
    }
}

impl Deref for Handle {
    type Target = gix::Repository;

    fn deref(&self) -> &gix::Repository {
        &self.repo
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        hook::closed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_user_scope_reads_user_config_but_no_git_environment() {
        let p = options(ConfigScope::User).permissions;
        assert!(
            !p.config.git_binary && !p.attributes.git_binary,
            "never runs git"
        );
        assert!(!p.config.env, "GIT_CONFIG_* has no effect");
        assert_eq!(p.env.git_prefix, Permission::Deny);
        assert_eq!(p.env.objects, Permission::Deny);
        assert!(p.config.user && p.config.system && p.config.git);
        assert!(
            options(ConfigScope::RepositoryOnly)
                .permissions
                .is_isolated()
        );
    }
}

/// Counting hook: how many handles this thread has opened and how many are
/// still alive. Compiled only into tests.
#[cfg(test)]
pub(crate) mod hook {
    use std::cell::Cell;

    thread_local! {
        static OPENED: Cell<usize> = const { Cell::new(0) };
        static LIVE: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn opened() {
        OPENED.with(|c| c.set(c.get() + 1));
        LIVE.with(|c| c.set(c.get() + 1));
    }

    pub(crate) fn closed() {
        LIVE.with(|c| c.set(c.get() - 1));
    }

    pub(crate) fn opened_count() -> usize {
        OPENED.with(Cell::get)
    }

    pub(crate) fn live_count() -> usize {
        LIVE.with(Cell::get)
    }
}

#[cfg(not(test))]
mod hook {
    pub(crate) fn opened() {}
    pub(crate) fn closed() {}
}
