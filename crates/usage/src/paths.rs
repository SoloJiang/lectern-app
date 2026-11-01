//! Where a changed file lies (docs/design/usage.md「项目标识」「轮次的观察与改动项」):
//! the repository it is in (any worktree of it is the same place), or the
//! directory it is in when it is in none. Blocking: `Projects::resolve`
//! reads the file system; executor only.

use std::path::{Path, PathBuf};

use lectern_services::project::{ProjectKey, Projects};

use crate::store::{PathPlace, Place};

/// How far up from a file's directory a directory that exists is looked
/// for (the directories of a new file may not exist yet, nor those of a
/// file whose worktree is gone).
const CLIMB: usize = 16;

/// Where the files of one directory lie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirPlace {
    place: Place,
    worktree: Option<PathBuf>,
    /// The directory below the worktree (or folder root).
    rel_dir: PathBuf,
}

impl DirPlace {
    /// The place of the file `name` in the directory.
    pub(crate) fn file(&self, name: &Path) -> PathPlace {
        PathPlace {
            place: self.place.clone(),
            worktree: self.worktree.clone(),
            rel: self.rel_dir.join(name),
        }
    }
}

/// The place of a directory. The nearest directory at or above it that
/// `resolve` knows decides the project; the directories below that one,
/// which do not exist, stay in the path.
pub(crate) fn dir_place(projects: Option<&dyn Projects>, dir: &Path) -> DirPlace {
    if let Some(projects) = projects {
        for ancestor in dir.ancestors().take(CLIMB) {
            let Ok(found) = projects.resolve(ancestor) else {
                continue;
            };
            let below = dir.strip_prefix(ancestor).unwrap_or(Path::new(""));
            // `join` leaves a trailing separator after an empty part.
            let rel_dir: PathBuf = Path::new(&*found.rel).join(below).components().collect();
            let place = match &found.project {
                ProjectKey::Git { common_dir } => Place::Git(common_dir.to_path_buf()),
                ProjectKey::Folder { root } => Place::Dir(root.join(&rel_dir)),
            };
            return DirPlace {
                place,
                worktree: found.worktree.as_ref().map(|w| w.0.to_path_buf()),
                rel_dir,
            };
        }
    }
    DirPlace {
        place: Place::Dir(dir.to_path_buf()),
        worktree: None,
        rel_dir: PathBuf::new(),
    }
}

/// The directory of a file and its name.
pub(crate) fn split(path: &Path) -> (&Path, &Path) {
    (
        path.parent().unwrap_or(path),
        path.file_name().map(Path::new).unwrap_or(Path::new("")),
    )
}
