//! 权限请求：agent 的请求、预览和可选的回答（docs/design/conversation.md「服务接口」）。

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::super::session::Timestamp;
use super::snapshot::CallKind;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// 提供者分配，本次运行内唯一。
    pub id: PermissionId,
    pub kind: CallKind,
    /// agent 写的工具调用标题，作为 agent 的内容原样显示。
    pub title: Arc<str>,
    pub detail: PermissionDetail,
    /// 每种至多一个，按 `OptionKind` 的顺序；agent 给了同一种的多个选项时只留第一个。
    pub options: Arc<[PermissionOption]>,
    pub asked_at: Timestamp,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct PermissionId(pub u64);

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PermissionDetail {
    /// 执行命令：agent 原始输入里的命令文本（ADR 0050），至多 16 KiB；`workdir` 是输入里的
    /// 工作目录。
    Command {
        text: Arc<str>,
        workdir: Option<Arc<Path>>,
    },
    /// 改文件：工具调用内容里的 diff，至多 8 个文件。
    Edits(Arc<[EditPreview]>),
    /// agent 没有给出可识别的内容，只有标题。
    None,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EditPreview {
    pub path: Arc<Path>,
    pub new_file: bool,
    pub added: u32,
    pub removed: u32,
    /// 至多 120 行（上下文 3 行）；其余计入 `omitted`。
    pub rows: Arc<[PreviewRow]>,
    pub omitted: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PreviewRow {
    pub kind: RowKind,
    pub text: Arc<str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RowKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PermissionOption {
    pub id: OptionId,
    pub kind: OptionKind,
}

/// agent 给的选项 ID，原样交回。不显示，不进日志。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct OptionId(pub Arc<str>);

/// 顺序即按钮的顺序。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum OptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}
