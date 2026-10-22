//! 发现、监听、归位、索引缓存、补齐与插件的测试（docs/design/agents.md「测试」，验收第 4 步）。
//!
//! 经插件的公开接口驱动：真实的 `Kernel`、`Executor::manual()`、`fs_watch` 的内存后端与手动驱动（不用真实的
//! FSEvents）、临时目录里的 `cache`，以及模拟的 `Projects`。会话文件是合成的 fixtures 和测试里生成的会话。

use std::path::Path;

mod complete;
mod discover;
mod index_cache;
mod place;
mod plugin;
mod spawn_link;
pub(crate) mod support;
mod unretained;
mod watch;

/// 递归复制一个 fixtures 目录（只复制文件，不复制 `SOURCE.md`）。
pub(crate) fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&path, &target);
        } else if entry.file_name() != "SOURCE.md" {
            std::fs::create_dir_all(to).unwrap();
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

pub(crate) fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}
