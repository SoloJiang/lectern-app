//! 解析计数（docs/design/agents.md「状态与异常」）。

use std::sync::Arc;

use lectern_services::session::{Diagnostics, Format};

/// 每种名字最多记这么多种（`Diagnostics::unknown_types` 的上限）。
const MAX_KINDS: usize = 32;

/// 一次读取（或一次摘要）里被跳过的行，以及归入 `ToolKind::Other` 的工具名。
/// 第 4 步把它并进会话与全局的 `Diagnostics`，`unknown_tools` 只进日志。
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct ParseStats {
    /// 不认识的记录类型及其行数，最多 32 种。
    pub unknown_types: Vec<(Arc<str>, u32)>,
    /// 不是合法 JSON，或缺少必需字段。
    pub bad_lines: u32,
    /// 超过 16 MB 的行。
    pub oversized_lines: u32,
    /// 分类表之外的工具名（归 `ToolKind::Other`）及其调用次数，最多 32 种。
    pub unknown_tools: Vec<(Arc<str>, u32)>,
}

impl ParseStats {
    /// 有行被跳过：`Format::Partial`。
    pub fn skipped(&self) -> bool {
        self.bad_lines > 0 || self.oversized_lines > 0 || !self.unknown_types.is_empty()
    }

    /// 没有被跳过的行时为 `Known`，否则为 `Partial`。
    pub fn format(&self) -> Format {
        if self.skipped() {
            Format::Partial
        } else {
            Format::Known
        }
    }

    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            unknown_types: self.unknown_types.clone().into(),
            bad_lines: self.bad_lines,
            oversized_lines: self.oversized_lines,
            unreadable_dirs: Arc::new([]),
        }
    }

    pub fn merge(&mut self, other: &ParseStats) {
        for (name, n) in &other.unknown_types {
            bump(&mut self.unknown_types, name, *n);
        }
        for (name, n) in &other.unknown_tools {
            bump(&mut self.unknown_tools, name, *n);
        }
        self.bad_lines = self.bad_lines.saturating_add(other.bad_lines);
        self.oversized_lines = self.oversized_lines.saturating_add(other.oversized_lines);
    }

    pub(crate) fn unknown_type(&mut self, name: &str) {
        bump(&mut self.unknown_types, name, 1);
    }

    pub(crate) fn unknown_tool(&mut self, name: &str) {
        bump(&mut self.unknown_tools, name, 1);
    }
}

fn bump(counts: &mut Vec<(Arc<str>, u32)>, name: &str, n: u32) {
    if let Some((_, count)) = counts.iter_mut().find(|(k, _)| &**k == name) {
        *count = count.saturating_add(n);
    } else if counts.len() < MAX_KINDS {
        counts.push((name.into(), n));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_stats_cap_kinds_and_merge() {
        let mut a = ParseStats::default();
        assert_eq!(a.format(), Format::Known);
        for i in 0..40 {
            a.unknown_type(&format!("t{i}"));
        }
        a.unknown_type("t0");
        assert_eq!(a.unknown_types.len(), MAX_KINDS);
        assert_eq!(a.unknown_types[0], ("t0".into(), 2));
        assert_eq!(a.format(), Format::Partial);

        let mut b = ParseStats::default();
        b.unknown_tool("WebFetch");
        assert!(!b.skipped(), "unknown tools do not skip records");
        b.bad_lines = 2;
        a.merge(&b);
        assert_eq!(a.bad_lines, 2);
        assert_eq!(a.unknown_tools, [("WebFetch".into(), 1)]);
        assert_eq!(a.diagnostics().unknown_types.len(), MAX_KINDS);
    }
}
