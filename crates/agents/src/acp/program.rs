//! 哪个 ACP 程序对应哪种 agent，以及在登录环境的 PATH 里查找它
//! （docs/design/agents.md「ACP 客户端」的「程序查找」，ADR 0050）。

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;

use lectern_platform::login_env::{find_program, login_env};
use lectern_services::conversation::AcpProgram;
use lectern_services::session::SourceId;

use crate::{claude, codex};

/// 程序按来源固定映射；其他来源没有 ACP 程序（ADR 0050：来源、ACP 消息和设置都不能给出程序）。
pub(crate) fn program_of_source(source: &SourceId) -> Option<AcpProgram> {
    match &*source.0 {
        claude::SOURCE_ID => Some(AcpProgram::ClaudeCode),
        codex::SOURCE_ID => Some(AcpProgram::Codex),
        _ => None,
    }
}

/// 36 个字符，8-4-4-4-12，小写十六进制（与 `terminal` 对恢复命令的检查相同）。
pub(crate) fn is_canonical_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(at, &b)| match at {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// 查找用的环境。
#[derive(Clone)]
pub(crate) enum Env {
    /// 用户登录 shell 的环境（`platform::login_env`，本次运行取一次，与 `terminal` 共用）；
    /// 取不到时退回 Lectern 进程的 `PATH`。
    Login,
    /// 固定的变量（测试和不想运行登录 shell 的装配）。
    Fixed(Arc<[(OsString, OsString)]>),
}

impl Env {
    /// 在这份环境的 `PATH` 里找 `program`。阻塞：`Login` 第一次调用时运行用户的登录 shell（至多 5 秒），
    /// 再 `stat`。只在 executor 上调用。
    pub(crate) fn find(&self, program: AcpProgram) -> Option<PathBuf> {
        let name = program.name();
        match self {
            Env::Login => match login_env() {
                Ok(env) => env.get("PATH").and_then(|path| find_program(name, path)),
                Err(_) => process_path().and_then(|path| find_program(name, &path)),
            },
            Env::Fixed(vars) => vars
                .iter()
                .rev()
                .find(|(var, _)| var == OsStr::new("PATH"))
                .and_then(|(_, path)| find_program(name, path)),
        }
    }
}

fn process_path() -> Option<OsString> {
    std::env::var_os("PATH")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_follow_the_source() {
        let claude_source = SourceId(claude::SOURCE_ID.into());
        let codex_source = SourceId(codex::SOURCE_ID.into());
        assert_eq!(
            program_of_source(&claude_source),
            Some(AcpProgram::ClaudeCode)
        );
        assert_eq!(program_of_source(&codex_source), Some(AcpProgram::Codex));
        assert_eq!(
            program_of_source(&SourceId("ext.somebody.session".into())),
            None
        );
        // The names are the adapters' `bin` entries (docs/design/agents.md「核对」).
        assert_eq!(AcpProgram::ClaudeCode.name(), "claude-agent-acp");
        assert_eq!(AcpProgram::Codex.name(), "codex-acp");
    }

    #[test]
    fn uuid_must_be_canonical() {
        assert!(is_canonical_uuid("5e55e55e-1111-4111-8111-0000000000a1"));
        for bad in [
            "",
            "5E55E55E-1111-4111-8111-0000000000A1",
            "5e55e55e11114111811100000000 00a1",
            "5e55e55e-1111-4111-8111-0000000000a1f",
            "--help",
            "5e55e55e-1111-4111-8111-0000000000a",
        ] {
            assert!(!is_canonical_uuid(bad), "{bad:?}");
        }
    }
}
