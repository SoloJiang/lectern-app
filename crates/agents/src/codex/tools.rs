//! Codex 工具名到 `ToolKind` 的分类表、补丁里的路径和工具输出的状态（docs/design/agents.md「记录」）。

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::session::{ToolKind, ToolStatus};
use serde::Deserialize;
use serde_json::value::RawValue;

use super::format::Text;
use crate::path::absolute;
use crate::stats::ParseStats;

/// 工具名属于哪一类。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Class {
    /// `apply_patch`：路径取补丁里的文件头。
    Patch,
    /// `shell`：`command` 是数组。
    Shell,
    /// `shell_command`：`command` 是字符串。
    ShellCommand,
    /// `exec_command`：`cmd`、`workdir`；可能打开交互式 shell 会话。
    ExecCommand,
    /// `write_stdin`：写进 `session_id` 这个交互式 shell 会话的字符。
    WriteStdin,
    /// `view_image`：路径取 `path`。
    ViewImage,
    SpawnAgent,
    Other,
}

pub(super) fn class(name: &str) -> Class {
    match name {
        "apply_patch" => Class::Patch,
        "shell" => Class::Shell,
        "shell_command" => Class::ShellCommand,
        "exec_command" => Class::ExecCommand,
        "write_stdin" => Class::WriteStdin,
        "view_image" => Class::ViewImage,
        "spawn_agent" => Class::SpawnAgent,
        _ => Class::Other,
    }
}

/// `function_call.arguments` 里分类要用的字段。值的类型不对时当作没有。
#[derive(Deserialize, Default)]
struct Args<'a> {
    #[serde(borrow, default)]
    command: Command<'a>,
    #[serde(borrow, default)]
    cmd: Lenient<'a>,
    #[serde(borrow, default)]
    workdir: Lenient<'a>,
    #[serde(borrow, default)]
    chars: Lenient<'a>,
    #[serde(borrow, default)]
    path: Lenient<'a>,
    #[serde(borrow, default)]
    input: Lenient<'a>,
    #[serde(default)]
    session_id: SessionId,
}

/// 字符串就留下，否则丢弃。
#[derive(Default)]
struct Lenient<'a>(Option<Cow<'a, str>>);

impl<'de: 'a, 'a> Deserialize<'de> for Lenient<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Any::deserialize(d)? {
            Any::Str(s) => Lenient(Some(s)),
            _ => Lenient(None),
        })
    }
}

/// `command`：字符串或字符串数组。
#[derive(Default)]
enum Command<'a> {
    Text(Cow<'a, str>),
    Argv(Vec<Cow<'a, str>>),
    #[default]
    None,
}

impl<'de: 'a, 'a> Deserialize<'de> for Command<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Any::deserialize(d)? {
            Any::Str(s) => Command::Text(s),
            Any::Strs(v) => Command::Argv(v),
            _ => Command::None,
        })
    }
}

/// `write_stdin.session_id`：数字或字符串，统一成十进制文本。
#[derive(Default)]
struct SessionId(Option<String>);

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Any::deserialize(d)? {
            Any::Str(s) => SessionId(Some(s.into_owned())),
            Any::Int(n) => SessionId(Some(n.to_string())),
            _ => SessionId(None),
        })
    }
}

/// 宽松读取任意 JSON 值：只认字符串、字符串数组和整数，其余跳过。
enum Any<'a> {
    Str(Cow<'a, str>),
    Strs(Vec<Cow<'a, str>>),
    Int(i128),
    Other,
}

impl<'de: 'a, 'a> Deserialize<'de> for Any<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
        use std::fmt;
        use std::marker::PhantomData;
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Any<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(Any::Str(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Any::Str(Cow::Owned(v.to_owned())))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(Any::Int(i128::from(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Any::Int(i128::from(v)))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(Any::Other)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(Any::Other)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Any::Other)
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut out = Vec::new();
                let mut strings = true;
                while let Some(item) = seq.next_element::<Any<'de>>()? {
                    match item {
                        Any::Str(s) if strings => out.push(s),
                        _ => strings = false,
                    }
                }
                Ok(if strings { Any::Strs(out) } else { Any::Other })
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Any::Other)
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// 一次工具调用的分类结果。
pub(super) struct Classified {
    pub kind: ToolKind,
    /// `exec_command` 的工作目录：结果里返回 shell 会话 ID 时，记下这个会话从哪个目录启动。
    pub exec_workdir: Option<Arc<Path>>,
}

/// `function_call` / `custom_tool_call` 的分类。`raw` 是 `arguments`（JSON 文本）或 `input`（补丁文本），
/// 都是原样的 JSON 字符串字面量。`sessions` 查交互式 shell 会话的启动目录。表外的工具名归 `Other` 并计数。
pub(super) fn kind(
    name: &str,
    raw: Option<&RawValue>,
    custom: bool,
    cwd: &Path,
    sessions: impl Fn(&str) -> Option<Arc<Path>>,
    stats: &mut ParseStats,
) -> Classified {
    let class = class(name);
    let plain = |kind| Classified {
        kind,
        exec_workdir: None,
    };
    match class {
        Class::SpawnAgent => return plain(ToolKind::SpawnAgent),
        Class::Other => {
            stats.unknown_tool(name);
            return plain(ToolKind::Other);
        }
        _ => {}
    }
    // 两种调用的载荷都是 JSON 字符串字面量：先取出字符串。
    let text: Option<Cow<'_, str>> = raw.and_then(|r| serde_json::from_str(r.get()).ok());
    if class == Class::Patch && custom {
        return plain(ToolKind::Edit {
            paths: patch_paths(text.as_deref().unwrap_or_default(), cwd).into(),
        });
    }
    let args: Args = text
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default();
    let workdir = |w: &Lenient<'_>| -> Arc<Path> {
        match w.0.as_deref() {
            Some(w) => absolute(cwd, w).into(),
            None => cwd.into(),
        }
    };
    match class {
        Class::Patch => plain(ToolKind::Edit {
            paths: patch_paths(args.input.0.as_deref().unwrap_or_default(), cwd).into(),
        }),
        Class::Shell | Class::ShellCommand => plain(ToolKind::Execute {
            command: match &args.command {
                Command::Text(t) => t.as_ref().into(),
                Command::Argv(argv) => script(argv).into(),
                Command::None => "".into(),
            },
            workdir: workdir(&args.workdir),
            exec_session: None,
            workdir_exact: true,
        }),
        Class::ExecCommand => {
            let dir = workdir(&args.workdir);
            Classified {
                kind: ToolKind::Execute {
                    command: args.cmd.0.as_deref().unwrap_or_default().into(),
                    workdir: dir.clone(),
                    exec_session: None,
                    workdir_exact: true,
                },
                exec_workdir: Some(dir),
            }
        }
        Class::WriteStdin => {
            let session = args.session_id.0;
            let dir = session
                .as_deref()
                .and_then(&sessions)
                .unwrap_or_else(|| cwd.into());
            plain(ToolKind::Execute {
                command: args.chars.0.as_deref().unwrap_or_default().into(),
                workdir: dir,
                exec_session: session.map(Arc::from),
                workdir_exact: false,
            })
        }
        Class::ViewImage => plain(ToolKind::Read {
            paths: args
                .path
                .0
                .as_deref()
                .map(|p| vec![absolute(cwd, p)])
                .unwrap_or_default()
                .into(),
        }),
        Class::SpawnAgent | Class::Other => unreachable!("returned above"),
    }
}

/// `local_shell_call` 的分类。
pub(super) fn local_shell(argv: &[Cow<'_, str>], workdir: Option<&str>, cwd: &Path) -> ToolKind {
    ToolKind::Execute {
        command: script(argv).into(),
        workdir: workdir.map_or_else(|| cwd.into(), |w| absolute(cwd, w).into()),
        exec_session: None,
        workdir_exact: true,
    }
}

/// 交给 shell 的完整文本：`["bash", "-lc", 脚本]` 一类取脚本，其余按 POSIX shell 的写法加引号拼接。
pub(super) fn script(argv: &[Cow<'_, str>]) -> String {
    if let [shell, flag, script] = argv {
        let name = shell.rsplit('/').next().unwrap_or(shell);
        if matches!(name, "bash" | "sh" | "zsh" | "dash") && matches!(&**flag, "-c" | "-lc") {
            return script.to_string();
        }
    }
    let mut out = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let safe = !arg.is_empty()
            && arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
        if safe {
            out.push_str(arg);
        } else {
            out.push('\'');
            out.push_str(&arg.replace('\'', r"'\''"));
            out.push('\'');
        }
    }
    out
}

/// 补丁里的一个文件。
pub(super) struct PatchFile<'p> {
    pub op: PatchOp,
    pub path: &'p str,
    pub move_to: Option<&'p str>,
    /// `Add` 的内容行（去掉 `+`），`Update` 的 hunk 行（原样）。
    pub body: Vec<&'p str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PatchOp {
    Add,
    Update,
    Delete,
}

/// 按 `*** Add File:`、`*** Update File:`、`*** Delete File:`、`*** Move to:` 切出补丁里的文件。
pub(super) fn patch_files(patch: &str) -> Vec<PatchFile<'_>> {
    let mut out: Vec<PatchFile<'_>> = Vec::new();
    for line in patch.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let header = |prefix: &str| line.strip_prefix(prefix).map(str::trim);
        if let Some(p) = header("*** Add File:") {
            out.push(PatchFile {
                op: PatchOp::Add,
                path: p,
                move_to: None,
                body: Vec::new(),
            });
        } else if let Some(p) = header("*** Update File:") {
            out.push(PatchFile {
                op: PatchOp::Update,
                path: p,
                move_to: None,
                body: Vec::new(),
            });
        } else if let Some(p) = header("*** Delete File:") {
            out.push(PatchFile {
                op: PatchOp::Delete,
                path: p,
                move_to: None,
                body: Vec::new(),
            });
        } else if let Some(p) = header("*** Move to:") {
            if let Some(f) = out.last_mut() {
                f.move_to = Some(p);
            }
        } else if line.starts_with("*** ") {
            // `*** Begin Patch`、`*** End Patch`、`*** End of File`。
        } else if let Some(f) = out.last_mut() {
            match f.op {
                PatchOp::Add => f.body.push(line.strip_prefix('+').unwrap_or(line)),
                PatchOp::Update => f.body.push(line),
                PatchOp::Delete => {}
            }
        }
    }
    out
}

/// 补丁改到的全部路径（含移动的目标），按 `cwd` 拼成绝对路径。
pub(super) fn patch_paths(patch: &str, cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for f in patch_files(patch) {
        out.push(absolute(cwd, f.path));
        if let Some(to) = f.move_to {
            out.push(absolute(cwd, to));
        }
    }
    out
}

/// 从工具输出得到的状态和打开的交互式 shell 会话。
pub(super) struct Outcome {
    pub exit_code: Option<i64>,
    pub exec_session: Option<Arc<str>>,
}

/// 输出开头（`Output:` 之前）的元数据行，或 JSON 输出里的 `exit_code` / `session_id`。
pub(super) fn outcome(text: &Text<'_>) -> Outcome {
    let mut out = Outcome {
        exit_code: None,
        exec_session: None,
    };
    let head = &*text.head;
    if head.starts_with('{') {
        // 旧版本 shell 的 `{"output": …, "metadata": {"exit_code": …}}`（元数据在末尾），
        // 或 code mode 的 `{…, "exit_code": …, "session_id": …, "output": …}`（在开头）。
        for part in [head, &*text.tail] {
            out.exit_code = out.exit_code.or_else(|| json_int(part, "\"exit_code\":"));
            if out.exec_session.is_none() {
                out.exec_session = json_int(part, "\"session_id\":").map(|n| n.to_string().into());
            }
        }
        return out;
    }
    for line in head.lines().take(16) {
        if line == "Output:" {
            break;
        }
        let number = |prefix: &str| -> Option<&str> {
            let rest = line.strip_prefix(prefix)?.trim();
            (!rest.is_empty()).then_some(rest)
        };
        if let Some(n) = number("Exit code:").or_else(|| number("Process exited with code")) {
            out.exit_code = n.parse().ok().or(out.exit_code);
        } else if let Some(id) = number("Process running with session ID") {
            out.exec_session = Some(id.into());
        }
    }
    out
}

/// `key` 之后的整数（可以有空白和负号）。
fn json_int(s: &str, key: &str) -> Option<i64> {
    let rest = s[s.find(key)? + key.len()..].trim_start();
    let end = rest
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && c == '-')))
        .map_or(rest.len(), |(i, _)| i);
    rest[..end].parse().ok()
}

/// 结果状态（「记录」）：有退出码时非 0 为 `Failed`；没有退出码时 `success == false` 为 `Failed`。
pub(super) fn status(outcome: &Outcome, success: Option<bool>) -> ToolStatus {
    match outcome.exit_code {
        Some(0) => ToolStatus::Completed,
        Some(_) => ToolStatus::Failed,
        None if success == Some(false) => ToolStatus::Failed,
        None => ToolStatus::Completed,
    }
}

/// `patch_apply_end` / `FileChange` 的状态。
pub(super) fn patch_status(status: Option<&str>, success: Option<bool>) -> Option<ToolStatus> {
    match status {
        Some("completed") => Some(ToolStatus::Completed),
        Some("failed") => Some(ToolStatus::Failed),
        Some("declined") => Some(ToolStatus::Denied),
        _ => match success {
            Some(true) => Some(ToolStatus::Completed),
            Some(false) => Some(ToolStatus::Failed),
            None => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Text<'static> {
        let json = serde_json::to_string(s).unwrap();
        let t: Text<'_> = serde_json::from_str(&json).unwrap();
        Text {
            head: Cow::Owned(t.head.into_owned()),
            tail: Cow::Owned(t.tail.into_owned()),
            blank: t.blank,
        }
    }

    #[test]
    fn tool_kind_codex_shell_scripts_and_quoting() {
        let argv = |v: &[&str]| -> Vec<Cow<'static, str>> {
            v.iter().map(|s| Cow::Owned((*s).to_owned())).collect()
        };
        assert_eq!(script(&argv(&["bash", "-lc", "cd a && ls"])), "cd a && ls");
        assert_eq!(script(&argv(&["/bin/zsh", "-c", "pwd"])), "pwd");
        assert_eq!(
            script(&argv(&["git", "commit", "-m", "it's done"])),
            r"git commit -m 'it'\''s done'"
        );
        assert_eq!(script(&argv(&["echo", ""])), "echo ''");
    }

    #[test]
    fn tool_kind_codex_patch_files() {
        let patch = "*** Begin Patch\n*** Add File: a.txt\n+one\n+two\n*** Update File: src/b.rs\n*** Move to: src/c.rs\n@@ fn x\n-old\n+new\n*** End of File\n*** Delete File: d.txt\n*** End Patch\n";
        let files = patch_files(patch);
        assert_eq!(files.len(), 3);
        assert_eq!((files[0].op, files[0].path), (PatchOp::Add, "a.txt"));
        assert_eq!(files[0].body, ["one", "two"]);
        assert_eq!(files[1].move_to, Some("src/c.rs"));
        assert_eq!(files[1].body, ["@@ fn x", "-old", "+new"]);
        assert_eq!(files[2].op, PatchOp::Delete);
        let cwd = Path::new("/Users/dev/fixture/repo");
        assert_eq!(
            patch_paths(patch, cwd),
            [
                cwd.join("a.txt"),
                cwd.join("src/b.rs"),
                cwd.join("src/c.rs"),
                cwd.join("d.txt")
            ]
        );
    }

    #[test]
    fn tool_kind_codex_output_outcomes() {
        let o = outcome(&text(
            "Exit code: 2\nWall time: 0.1 seconds\nOutput:\nExit code: 0",
        ));
        assert_eq!(o.exit_code, Some(2));
        assert_eq!(status(&o, Some(true)), ToolStatus::Failed);
        let o = outcome(&text(
            "Chunk ID: c1\nWall time: 1.0000 seconds\nProcess running with session ID 42\nOutput:\nx",
        ));
        assert_eq!((o.exit_code, o.exec_session.as_deref()), (None, Some("42")));
        assert_eq!(status(&o, None), ToolStatus::Completed);
        let o = outcome(&text(
            "Wall time: 0.2000 seconds\nProcess exited with code 0\nOutput:\n",
        ));
        assert_eq!(o.exit_code, Some(0));
        let long = format!(
            r#"{{"output":"{}","metadata":{{"exit_code":-1,"duration_seconds":0.5}}}}"#,
            "y".repeat(5000)
        );
        assert_eq!(outcome(&text(&long)).exit_code, Some(-1));
        let o = outcome(&text(
            r#"{"wall_time_seconds":1.0,"session_id":7,"output":""}"#,
        ));
        assert_eq!(o.exec_session.as_deref(), Some("7"));
        // 没有退出码时看 `success`。
        let o = outcome(&text("patched"));
        assert_eq!(status(&o, Some(false)), ToolStatus::Failed);
        assert_eq!(status(&o, None), ToolStatus::Completed);
    }
}
