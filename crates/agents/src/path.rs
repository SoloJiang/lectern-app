//! 路径的词法处理（不访问文件系统）。

use std::path::{Component, Path, PathBuf};

/// 相对路径按 `cwd` 在词法上拼接并去掉 `.`、`..`，不访问文件系统。空路径是 `cwd` 本身。
pub(crate) fn absolute(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` 就是 `/`。
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_kind_paths_are_lexically_absolute() {
        let cwd = Path::new("/Users/dev/fixture/repo");
        assert_eq!(
            absolute(cwd, "src/../lib/./a.rs"),
            Path::new("/Users/dev/fixture/repo/lib/a.rs")
        );
        assert_eq!(absolute(cwd, "/etc/hosts"), Path::new("/etc/hosts"));
        assert_eq!(absolute(cwd, ""), cwd);
        assert_eq!(absolute(Path::new("/"), "../x"), Path::new("/x"));
    }
}
