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

/// The place of the files of a directory. The nearest directory at or
/// above it that `resolve` knows decides the project; the directories below
/// that one, which do not exist, stay in the path of a folder's place.
pub(crate) fn dir_place(projects: Option<&dyn Projects>, dir: &Path) -> PathPlace {
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
            let rel = match &found.project {
                ProjectKey::Git { .. } => rel_dir.to_string_lossy().replace('\\', "/"),
                ProjectKey::Folder { .. } => String::new(),
            };
            return PathPlace {
                place,
                worktree: found.worktree.as_ref().map(|w| w.0.to_path_buf()),
                rel,
            };
        }
    }
    PathPlace {
        place: Place::Dir(dir.to_path_buf()),
        worktree: None,
        rel: String::new(),
    }
}

/// The directory of a file.
pub(crate) fn dir_of(path: &Path) -> &Path {
    path.parent().unwrap_or(path)
}
