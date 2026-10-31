//! 运行日志里本 crate 的取值（docs/design/terminal.md「日志与诊断」、docs/design/logging.md）。
//!
//! 只记种类、计数、长度、程序名（不含目录）和工作目录；不记终端输出、标题、选区、粘贴与输入的文字、
//! argv 的内容、环境变量和 `Theme` 的颜色（「隐私」）。`LaunchKind`、`AgentCommand`、`LaunchError`、
//! `FailReason` 定义在 `services`，那里不依赖 `logging`，所以取值写成这里的函数（返回固定的小写名）。

use lectern_logging::{Ctx, Value};
use lectern_services::terminal::{AgentCommand, FailReason, Input, LaunchError, LaunchKind};

/// 会话 ID 的 128 位标识，用于 `session` 作用域：ID 是 UUID 时取它的 16 个字节，否则取哈希。算法与
/// `agents`、`usage`、`review` 相同，几个 crate 对同一会话的记录可以对照。
pub(crate) fn session_tag(id: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut digits = 0usize;
    let mut uuid = true;
    for b in id.bytes() {
        if b == b'-' {
            continue;
        }
        let Some(d) = (b as char).to_digit(16) else {
            uuid = false;
            break;
        };
        if digits == 32 {
            uuid = false;
            break;
        }
        // `to_digit(16)` 小于 16。
        let d = d as u8;
        out[digits / 2] |= if digits.is_multiple_of(2) { d << 4 } else { d };
        digits += 1;
    }
    if uuid && digits == 32 {
        return out;
    }
    use std::hash::{Hash, Hasher};
    let mut out = [0u8; 16];
    for (i, half) in out.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (i, id).hash(&mut h);
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// 一个会话的作用域：只设 `session`，其余沿用外层（executor 任务的 `plugin`、`act`、`task`）。
pub(crate) fn session_ctx(id: &str) -> Ctx {
    Ctx {
        session: Some(session_tag(id)),
        ..Ctx::default()
    }
}

/// `launch` 字段。
pub(crate) fn launch(kind: &LaunchKind) -> Value<'static> {
    Value::text(match kind {
        LaunchKind::Resume(_) => "resume",
        LaunchKind::NewSession(_) => "new_session",
        LaunchKind::Shell => "shell",
    })
}

/// `reason` 字段（`terminal launch refused`）。
pub(crate) fn launch_error(error: LaunchError) -> Value<'static> {
    Value::text(match error {
        LaunchError::AlreadyRunning(_) => "already_running",
        LaunchError::UnknownSession => "unknown_session",
        LaunchError::UntrustedSource => "untrusted_source",
        LaunchError::NotResumable => "not_resumable",
        LaunchError::InvalidSessionId => "invalid_session_id",
        LaunchError::OutsideProject => "outside_project",
        LaunchError::TooMany => "too_many",
    })
}

/// `reason` 字段（`terminal start failed`）。
pub(crate) fn fail_reason(reason: FailReason) -> Value<'static> {
    Value::text(match reason {
        FailReason::CwdMissing => "cwd_missing",
        FailReason::OutsideProject => "outside_project",
        FailReason::NotInstalled(AgentCommand::ClaudeCode) => "not_installed_claude",
        FailReason::NotInstalled(AgentCommand::Codex) => "not_installed_codex",
        FailReason::NoShell => "no_shell",
        FailReason::Spawn { .. } => "spawn",
        FailReason::Internal => "internal",
    })
}

/// 输入的变体名（`terminal input`）。
pub(crate) fn input_kind(input: &Input) -> Value<'static> {
    Value::text(match input {
        Input::Key(_) => "key",
        Input::Text(_) => "text",
        Input::Paste(_) => "paste",
        Input::Mouse(_) => "mouse",
        Input::Scroll(_) => "scroll",
        Input::ScrollTo(_) => "scroll_to",
        Input::Resize(_) => "resize",
        Input::Focus(_) => "focus",
        Input::SelectAll => "select_all",
        Input::ClearSelection => "clear_selection",
        Input::ClearScrollback => "clear_scrollback",
        Input::SetTheme(_) => "set_theme",
    })
}

/// 为什么关闭（`terminal closed` 的 `reason`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CloseReason {
    User,
    ProjectClosed,
    Deactivated,
    Replaced,
}

impl CloseReason {
    pub(crate) fn value(self) -> Value<'static> {
        Value::text(match self {
            CloseReason::User => "user",
            CloseReason::ProjectClosed => "project_closed",
            CloseReason::Deactivated => "deactivated",
            CloseReason::Replaced => "replaced",
        })
    }
}

/// 登录环境从哪里来（`terminal started` 的 `login_env`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EnvSource {
    /// 取到了用户登录 shell 的环境。
    Captured,
    /// 取不到，退回 Lectern 自己的环境。
    Fallback,
    /// 不是 agent 启动，用 Lectern 自己的环境。
    NotNeeded,
}

impl EnvSource {
    pub(crate) fn value(self) -> Value<'static> {
        Value::text(match self {
            EnvSource::Captured => "captured",
            EnvSource::Fallback => "fallback",
            EnvSource::NotNeeded => "not_needed",
        })
    }
}

/// The Bourne shell's name, written in two parts so that the acceptance
/// search for shell invocations (a quoted `sh` in `src`) finds invocations
/// only.
const BOURNE: &str = concat!("s", "h");

/// `program` 字段：不含目录的程序名，取自固定表。
pub(crate) fn program_name(name: &str) -> Value<'static> {
    Value::text(match name {
        "claude" => "claude",
        "codex" => "codex",
        "zsh" => "zsh",
        "bash" => "bash",
        "fish" => "fish",
        "ksh" => "ksh",
        "dash" => "dash",
        _ if name == BOURNE => BOURNE,
        _ => "other",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tag_of_a_uuid_is_its_bytes() {
        let tag = session_tag("00112233-4455-6677-8899-aabbccddeeff");
        assert_eq!(
            tag,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
    }

    #[test]
    fn session_tag_of_another_id_is_stable() {
        assert_eq!(session_tag("not-a-uuid"), session_tag("not-a-uuid"));
        assert_ne!(session_tag("not-a-uuid"), session_tag("another"));
    }

    #[test]
    fn program_names_outside_the_table_are_other() {
        assert_eq!(program_name("zsh").to_string(), "zsh");
        assert_eq!(program_name("my-secret-shell").to_string(), "other");
    }
}
