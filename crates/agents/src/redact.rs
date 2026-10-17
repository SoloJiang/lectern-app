//! 测试辅助：按字段白名单把真实会话日志脱敏成 fixture（docs/design/agents.md「外部格式」末尾的 fixtures）。
//!
//! 保留结构、键名、记录类型这类取值、数字与布尔、时间戳和 token 数；ID 换成形状相同的假 ID，
//! 同一个 ID 总换成同一个值，所以记录之间的引用关系不变；路径统一到 `/Users/dev/fixture/` 下，
//! 每一级目录名换成等长的占位名（同名同换，`.claude`、`subagents` 一类的结构性名字和扩展名保留）；
//! 其余文本换成等长的占位内容，保留空白和解析器依赖的开头标记。
//!
//! 现在的 fixtures 是合成的；人提供真实日志之后，用被忽略的测试 `redact_file_from_env` 生成：
//!
//! ```sh
//! LECTERN_REDACT_IN=<真实会话.jsonl> LECTERN_REDACT_OUT=<fixture.jsonl> \
//!   cargo test -p lectern-agents redact_file_from_env -- --ignored
//! ```
//!
//! 生成之后仍要逐行看过再提交。

use std::collections::HashMap;

use serde_json::Value;

/// 字符串原样保留的键：结构与枚举取值，不含用户内容。
const KEEP: &[&str] = &[
    "type",
    "timestamp",
    "version",
    "userType",
    "stop_reason",
    "stop_sequence",
    "role",
    "model",
    "advisorModel",
    "subtype",
    "service_tier",
    "kind",
    "agentType",
    "level",
    "status",
    "media_type",
    // Codex：模式、版本、中止原因、额度的标识。
    "history_mode",
    "cli_version",
    "originator",
    "plan_type",
    "limit_id",
];

/// 值是任意 JSON 的键（工具输入、工具结果）：里面的字符串一律不按 `KEEP` 保留，`name`、`type` 这类键也可能装着用户内容。
/// `tool_result` 块的 `content` 同样按任意内容处理。ID 与路径仍按各自的规则替换。
const PAYLOADS: &[&str] = &["input", "toolUseResult", "arguments"];

/// 值是 ID 的键。
const IDS: &[&str] = &[
    "uuid",
    "parentUuid",
    "logicalParentUuid",
    "sessionId",
    "agentId",
    "leafUuid",
    "messageId",
    "id",
    "tool_use_id",
    "toolUseId",
    "toolUseID",
    "parentToolUseID",
    "sourceToolUseID",
    "requestId",
    "promptId",
    // Codex。
    "call_id",
    "turn_id",
    "root_turn_id",
    "thread_id",
    "session_id",
    "response_id",
    "forked_from_id",
    "parent_thread_id",
];

/// 值是路径（或路径数组）的键。
const PATHS: &[&str] = &[
    "cwd",
    "file_path",
    "filePath",
    "notebook_path",
    "path",
    "worktreePath",
    "filenames",
    // Codex。
    "workdir",
    "working_directory",
    "move_path",
];

/// 值是分支名的键：按 `/` 分段，像路径的每一级一样替换。
const BRANCHES: &[&str] = &["gitBranch", "branch"];

/// 路径里保留原样的目录名。
const STRUCTURAL: &[&str] = &[
    ".claude",
    ".codex",
    "worktrees",
    "projects",
    "subagents",
    "workflows",
    "sessions",
    "src",
    "tests",
    "crates",
];

/// 文本开头保留的标记（解析器靠它们划分轮次和结果状态）。
const MARKERS: &[&str] = &[
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<local-command-caveat>",
    "<command-name>",
    "<task-notification>",
    "<tool_use_error>",
    "[Request interrupted by user for tool use]",
    "[Request interrupted by user]",
    "The user doesn't want to proceed with this tool use",
    "User rejected tool use",
    "Error: ",
];

/// Codex 工具输出开头的元数据行：整行保留（退出码和交互式 shell 会话 ID 决定结果状态和会话关联）。
const OUTPUT_HEADERS: &[&str] = &[
    "Exit code: ",
    "Process exited with code ",
    "Process running with session ID ",
];

/// 结构上保留工具名的条目类型。
const TOOL_CALLS: &[&str] = &["tool_use", "function_call", "custom_tool_call"];

const FIXTURE_ROOT: &str = "/Users/dev/fixture";

#[derive(Default)]
pub(crate) struct Redactor {
    ids: HashMap<String, String>,
    names: HashMap<String, String>,
}

impl Redactor {
    /// 脱敏一行 JSONL；不是合法 JSON 的行换成同样长度的占位文本（它本来就会被计为坏行）。
    pub(crate) fn line(&mut self, line: &str) -> String {
        match serde_json::from_str::<Value>(line) {
            Ok(mut v) => {
                self.value(None, &mut v, false);
                serde_json::to_string(&v).expect("a JSON value serializes")
            }
            Err(_) => text(line),
        }
    }

    /// `opaque`：当前位于任意内容里（见 `PAYLOADS`）。
    fn value(&mut self, key: Option<&str>, v: &mut Value, opaque: bool) {
        match v {
            Value::Object(map) => {
                let kind = map.get("type").and_then(Value::as_str).map(str::to_owned);
                for (k, v) in map.iter_mut() {
                    let inner = opaque
                        || PAYLOADS.contains(&k.as_str())
                        || (k == "content" && kind.as_deref() == Some("tool_result"));
                    // 工具名只在工具调用（`tool_use` 块、Codex 的 `function_call` / `custom_tool_call`）本身上保留。
                    if !opaque
                        && k == "name"
                        && kind.as_deref().is_some_and(|k| TOOL_CALLS.contains(&k))
                    {
                        continue;
                    }
                    // Codex 的 `arguments` 是装着 JSON 的字符串：按任意内容脱敏它里面的值，键保留。
                    if k == "arguments"
                        && let Value::String(text) = v
                        && let Ok(mut inner) = serde_json::from_str::<Value>(text)
                    {
                        self.value(None, &mut inner, true);
                        *text = serde_json::to_string(&inner).expect("a JSON value serializes");
                        continue;
                    }
                    // Codex 的工具输出：任意内容，开头的元数据行保留。
                    if !opaque && k == "output" {
                        self.output(v);
                        continue;
                    }
                    // Codex 的改动：键是路径。
                    if !opaque && k == "changes" && v.is_object() {
                        let Value::Object(changes) = std::mem::take(v) else {
                            unreachable!("checked above")
                        };
                        let mut out = serde_json::Map::new();
                        for (path, mut change) in changes {
                            self.value(None, &mut change, false);
                            out.insert(self.path(&path), change);
                        }
                        *v = Value::Object(out);
                        continue;
                    }
                    self.value(Some(k), v, inner);
                }
            }
            Value::Array(items) => {
                for item in items {
                    // `structuredPatch[].lines` 保留每行开头的 diff 标记。
                    if key == Some("lines")
                        && let Value::String(s) = item
                    {
                        *s = diff_line(s);
                        continue;
                    }
                    self.value(key, item, opaque);
                }
            }
            Value::String(s) => *s = self.string(key, s, opaque),
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    /// 工具输出（字符串、内容项数组或 `{content, success}`）：键、数字和布尔保留，字符串按 [`output_text`] 换掉；
    /// 字符串本身是 JSON（旧版本 shell 的 `{"output": …, "metadata": {"exit_code": …}}`）时按同样的规则处理里面的值。
    fn output(&mut self, v: &mut Value) {
        match v {
            Value::String(s) => {
                if let Ok(mut inner @ (Value::Object(_) | Value::Array(_))) =
                    serde_json::from_str(s)
                {
                    self.output(&mut inner);
                    *s = serde_json::to_string(&inner).expect("a JSON value serializes");
                } else {
                    *s = output_text(s);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|i| self.output(i)),
            Value::Object(map) => map.values_mut().for_each(|i| self.output(i)),
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    fn string(&mut self, key: Option<&str>, s: &str, opaque: bool) -> String {
        let Some(key) = key else {
            return text(s);
        };
        if !opaque && KEEP.contains(&key) {
            s.to_owned()
        } else if IDS.contains(&key) {
            self.id(s)
        } else if PATHS.contains(&key) {
            self.path(s)
        } else if BRANCHES.contains(&key) {
            s.split('/')
                .map(|p| self.name(p))
                .collect::<Vec<_>>()
                .join("/")
        } else {
            text(s)
        }
    }

    /// 同一个 ID 总是换成同一个假 ID：字母数字换成序号的数字，`-`、`_` 和开头的 `msg_` 一类前缀保留。
    pub(crate) fn id(&mut self, s: &str) -> String {
        if let Some(fake) = self.ids.get(s) {
            return fake.clone();
        }
        let (prefix, rest) = match s.split_once('_') {
            Some((p, rest)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase()) => {
                (format!("{p}_"), rest)
            }
            _ => (String::new(), s),
        };
        let slots = rest.chars().filter(char::is_ascii_alphanumeric).count();
        let n = self.ids.len() + 1;
        let digits = format!("{n:0>slots$}");
        let mut digits = digits[digits.len().saturating_sub(slots)..].chars();
        let body: String = rest
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    digits.next().unwrap_or('0')
                } else {
                    c
                }
            })
            .collect();
        let fake = prefix + &body;
        self.ids.insert(s.to_owned(), fake.clone());
        fake
    }

    /// 绝对路径去掉 `/Users/<名字>`、`/home/<名字>` 之后放到 `/Users/dev/fixture/` 下。
    pub(crate) fn path(&mut self, s: &str) -> String {
        let Some(rest) = s.strip_prefix('/') else {
            return s
                .split('/')
                .map(|p| self.name(p))
                .collect::<Vec<_>>()
                .join("/");
        };
        let mut parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 2 && matches!(parts[0], "Users" | "home") {
            parts.drain(..2);
        }
        let mapped: Vec<String> = parts.iter().map(|p| self.name(p)).collect();
        if mapped.iter().all(String::is_empty) {
            return FIXTURE_ROOT.to_owned();
        }
        format!("{FIXTURE_ROOT}/{}", mapped.join("/"))
    }

    /// 目录名或文件名换成等长的占位名，扩展名和结构性名字保留，同名同换。
    fn name(&mut self, part: &str) -> String {
        if part.is_empty() || STRUCTURAL.contains(&part) {
            return part.to_owned();
        }
        if let Some(fake) = self.names.get(part) {
            return fake.clone();
        }
        let (stem, ext) = match part.rfind('.') {
            Some(i) if i > 0 => part.split_at(i),
            _ => (part, ""),
        };
        let n = self.names.len();
        let len = stem.chars().count();
        // 序号写成字母（a、b、…、z、ba、…），不够长时左边补 `n`。
        let mut code = String::new();
        let mut k = n;
        loop {
            code.insert(0, char::from(b'a' + (k % 26) as u8));
            k /= 26;
            if k == 0 {
                break;
            }
        }
        let fake_stem = if code.len() >= len {
            code[code.len() - len..].to_owned()
        } else {
            "n".repeat(len - code.len()) + &code
        };
        let fake = fake_stem + ext;
        self.names.insert(part.to_owned(), fake.clone());
        fake
    }
}

/// 文本换成等长（按字符计）的占位内容：保留空白和开头的标记，其余字符换成 `x`。
pub(crate) fn text(s: &str) -> String {
    let (marker, rest) = MARKERS
        .iter()
        .find_map(|m| s.strip_prefix(m).map(|rest| (*m, rest)))
        .unwrap_or(("", s));
    let mut out = String::with_capacity(s.len());
    out.push_str(marker);
    out.extend(
        rest.chars()
            .map(|c| if c.is_whitespace() { c } else { 'x' }),
    );
    out
}

/// Codex 工具输出：`Output:` 之前的元数据行里，退出码和 shell 会话 ID 整行保留，其余按 [`text`] 换掉。
pub(crate) fn output_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut header = true;
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if header && (line == "Output:" || OUTPUT_HEADERS.iter().any(|h| line.starts_with(h))) {
            out.push_str(line);
        } else {
            out.push_str(&text(line));
        }
        if line == "Output:" {
            header = false;
        }
    }
    out
}

fn diff_line(s: &str) -> String {
    match s.chars().next() {
        Some(c @ ('+' | '-' | ' ')) => {
            let mut out = String::from(c);
            out.push_str(&text(&s[1..]));
            out
        }
        _ => text(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::tests::{TempDir, fixture_files, project_dir, temp_file};
    use crate::claude::{ClaudeCode, Owner, SOURCE_ID};
    use lectern_services::session::{Record, SourceId};

    #[test]
    fn redact_keeps_structure_numbers_and_whitelisted_values() {
        let mut r = Redactor::default();
        let line = r#"{"type":"assistant","timestamp":"2026-10-01T09:00:00.000Z","isSidechain":false,"message":{"model":"m-1","stop_reason":"end_turn","content":[{"type":"text","text":"secret words\nsecond"}],"usage":{"input_tokens":12,"output_tokens":34}},"extra":null}"#;
        let out: Value = serde_json::from_str(&r.line(line)).unwrap();
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(out["type"], v["type"]);
        assert_eq!(out["timestamp"], v["timestamp"]);
        assert_eq!(out["isSidechain"], v["isSidechain"]);
        assert_eq!(out["extra"], Value::Null);
        assert_eq!(out["message"]["model"], "m-1");
        assert_eq!(out["message"]["stop_reason"], "end_turn");
        assert_eq!(out["message"]["usage"], v["message"]["usage"]);
        assert_eq!(out["message"]["content"][0]["type"], "text");
        assert_eq!(out["message"]["content"][0]["text"], "xxxxxx xxxxx\nxxxxxx");
        assert_eq!(r.line("{broken"), "xxxxxxx");
    }

    #[test]
    fn redact_payloads_do_not_keep_structural_key_names() {
        let mut r = Redactor::default();
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"mcp__crm__lookup","input":{"name":"Acme Corp","type":"customer","status":"vip"}}]},"toolUseResult":{"name":"repo-x","kind":"private"}}"#;
        let out: Value = serde_json::from_str(&r.line(line)).unwrap();
        let block = &out["message"]["content"][0];
        assert_eq!(
            block["name"], "mcp__crm__lookup",
            "the tool name is structural"
        );
        assert_eq!(block["type"], "tool_use");
        for (v, original) in [
            (&block["input"]["name"], "Acme Corp"),
            (&block["input"]["type"], "customer"),
            (&block["input"]["status"], "vip"),
            (&out["toolUseResult"]["name"], "repo-x"),
            (&out["toolUseResult"]["kind"], "private"),
        ] {
            let v = v.as_str().unwrap();
            assert_ne!(v, original);
            assert_eq!(v.len(), original.len());
        }
        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","name":"leak","text":"out"}]}]}}"#;
        let out: Value = serde_json::from_str(&r.line(result)).unwrap();
        assert_ne!(out["message"]["content"][0]["content"][0]["name"], "leak");
    }

    #[test]
    fn redact_ids_keep_their_shape_and_relations() {
        let mut r = Redactor::default();
        let uuid = "3f2a9c1e-7b4d-4e8a-9c2f-1a2b3c4d5e6f";
        let a = r.id(uuid);
        let b = r.id("msg_01AbCdEf");
        assert_eq!(a, "00000000-0000-0000-0000-000000000001");
        assert_eq!(b, "msg_00000002");
        assert_eq!(r.id(uuid), a, "the same id maps to the same fake");
        assert_ne!(r.id("toolu_01AbCdEf"), b);
        assert_eq!(r.id("a1b2c3d"), "0000004");
    }

    #[test]
    fn redact_paths_move_under_the_fixture_root() {
        let mut r = Redactor::default();
        let cwd = r.path("/Users/alice/work/secret-project");
        let file = r.path("/Users/alice/work/secret-project/src/main.rs");
        let wt = r.path("/Users/alice/work/secret-project/.claude/worktrees/agent-1");
        assert_eq!(cwd, "/Users/dev/fixture/nnna/nnnnnnnnnnnnnb");
        assert!(file.starts_with(&format!("{cwd}/src/")), "{file}");
        assert!(file.ends_with(".rs"));
        assert_eq!(file.len(), cwd.len() + "/src/main.rs".len());
        assert!(wt.starts_with(&format!("{cwd}/.claude/worktrees/")), "{wt}");
        assert_eq!(r.path("/home/bob"), "/Users/dev/fixture");
        assert_eq!(
            r.path("src/main.rs"),
            "src/nnnc.rs",
            "the same name maps to the same placeholder"
        );
        assert!(r.path("/opt/tool/bin").starts_with("/Users/dev/fixture/"));
    }

    #[test]
    fn redact_text_keeps_length_whitespace_and_markers() {
        assert_eq!(text("héllo wörld"), "xxxxx xxxxx");
        assert_eq!(text("a\n\tb"), "x\n\tx");
        assert_eq!(
            text("<local-command-stdout>private</local-command-stdout>"),
            "<local-command-stdout>xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        );
        assert_eq!(
            text("[Request interrupted by user for tool use]"),
            "[Request interrupted by user for tool use]"
        );
        assert_eq!(diff_line("+added secret"), "+xxxxx xxxxxx");
        assert_eq!(diff_line(" context"), " xxxxxxx");
    }

    /// 脱敏前后解析出的记录只差在路径、ID 和文本上。
    fn shape(records: &[lectern_services::session::Positioned<Record>]) -> Vec<String> {
        records
            .iter()
            .map(|r| {
                let detail = match &r.value {
                    Record::Context { .. } => "context".to_owned(),
                    Record::TurnStarted { .. } => "turn".to_owned(),
                    Record::TurnEnded { .. } => "ended".to_owned(),
                    Record::ToolStarted(t) => {
                        format!("tool {} {:?}", t.name, std::mem::discriminant(&t.kind))
                    }
                    Record::ToolFinished(f) => format!("finished {:?}", f.status),
                    Record::Usage(u) => format!(
                        "usage {:?} {:?}",
                        u.tokens,
                        u.advisor.as_ref().map(|a| a.tokens)
                    ),
                    Record::RateLimits(_) => "limits".to_owned(),
                };
                format!("{} {} {detail}", r.seq, r.inherited)
            })
            .collect()
    }

    #[test]
    fn redact_fixtures_parse_to_the_same_shape() {
        let adapter = ClaudeCode::new(SourceId(SOURCE_ID.into()));
        for (name, rel) in fixture_files() {
            let original = adapter.session_file(&project_dir().join(&rel)).unwrap();
            let (expected, _) = adapter.parse_all(&original).unwrap();

            let mut r = Redactor::default();
            let text = std::fs::read_to_string(&original.path).unwrap();
            let redacted: String = text.lines().map(|l| r.line(l) + "\n").collect();
            let owner = match &original.owner {
                Owner::Main { session_id } => Owner::Main {
                    session_id: r.id(session_id).into(),
                },
                Owner::Subagent {
                    parent_session_id,
                    agent_id,
                } => Owner::Subagent {
                    parent_session_id: r.id(parent_session_id).into(),
                    agent_id: r.id(agent_id).into(),
                },
            };
            let dir = TempDir::new();
            let file = temp_file(&dir, owner);
            std::fs::write(&file.path, redacted).unwrap();
            let (actual, _) = adapter.parse_all(&file).unwrap();
            assert_eq!(
                shape(&actual.batch.records),
                shape(&expected.batch.records),
                "{name}"
            );
            assert_eq!(actual.stats, expected.stats, "{name}");
        }
    }

    #[test]
    fn redact_codex_keeps_ids_tool_names_and_output_headers() {
        let mut r = Redactor::default();
        let call = r#"{"timestamp":"2026-10-01T09:00:00.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cat secret.txt\",\"workdir\":\"/Users/alice/work/app\",\"name\":\"Acme\"}","call_id":"call_AbC123"}}"#;
        let out: Value = serde_json::from_str(&r.line(call)).unwrap();
        let payload = &out["payload"];
        assert_eq!(payload["name"], "exec_command");
        assert_eq!(payload["call_id"], r.id("call_AbC123"));
        let args: Value = serde_json::from_str(payload["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["cmd"], "xxx xxxxxxxxxx");
        assert!(
            args["workdir"]
                .as_str()
                .unwrap()
                .starts_with("/Users/dev/fixture/")
        );
        assert_ne!(args["name"], "Acme", "names inside arguments are content");

        let output = r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call_AbC123","output":"Chunk ID: c1\nWall time: 0.1 seconds\nProcess exited with code 2\nOutput:\nExit code: 0 secret"}}"#;
        let out: Value = serde_json::from_str(&r.line(output)).unwrap();
        assert_eq!(out["payload"]["call_id"], r.id("call_AbC123"));
        assert_eq!(
            out["payload"]["output"],
            "xxxxx xxx xx\nxxxx xxxxx xxx xxxxxxx\nProcess exited with code 2\nOutput:\nxxxx xxxxx x xxxxxx"
        );
        // 旧版本的 JSON 输出：数字保留。
        let old = r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"{\"output\":\"secret\",\"metadata\":{\"exit_code\":101}}"}}"#;
        let out: Value = serde_json::from_str(&r.line(old)).unwrap();
        let inner: Value =
            serde_json::from_str(out["payload"]["output"].as_str().unwrap()).unwrap();
        assert_eq!(inner["metadata"]["exit_code"], 101);
        assert_eq!(inner["output"], "xxxxxx");

        let patch = r#"{"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"c","changes":{"/Users/alice/work/app/src/a.rs":{"type":"update","unified_diff":"-secret\n+other\n","move_path":"/Users/alice/work/app/src/b.rs"}},"status":"completed"}}"#;
        let out: Value = serde_json::from_str(&r.line(patch)).unwrap();
        let changes = out["payload"]["changes"].as_object().unwrap();
        let (path, change) = changes.iter().next().unwrap();
        assert!(path.starts_with("/Users/dev/fixture/") && path.ends_with(".rs"));
        assert_eq!(change["type"], "update");
        assert_eq!(out["payload"]["status"], "completed");
        assert!(
            change["move_path"]
                .as_str()
                .unwrap()
                .starts_with("/Users/dev/fixture/")
        );
        assert_ne!(change["unified_diff"], "-secret\n+other\n");
    }

    #[test]
    fn redact_codex_fixtures_parse_to_the_same_shape() {
        use crate::codex::tests::{MapLineage, fixture_files, fixtures_dir, rollout_name};
        use crate::codex::{Codex, SOURCE_ID as CODEX};
        let base = || Codex::without_decoded_copies(SourceId(CODEX.into()));
        let originals = MapLineage::default();
        let redacted = MapLineage::default();
        let dir = TempDir::new();
        let mut r = Redactor::default();
        let mut pairs = Vec::new();
        for (name, rel) in fixture_files() {
            let original = base().session_file(&fixtures_dir().join(rel)).unwrap();
            originals.insert(&original.key.id, original.clone());
            let text = std::fs::read_to_string(&original.path).unwrap();
            let out: String = text.lines().map(|l| r.line(l) + "\n").collect();
            let thread = r.id(&original.key.id);
            let path = dir.path().join(rollout_name(&thread, false));
            std::fs::write(&path, out).unwrap();
            let file = base().session_file(&path).unwrap();
            redacted.insert(&thread, file.clone());
            pairs.push((name, original, file));
        }
        let originals = base().with_lineage(std::sync::Arc::new(originals));
        let redacted = base().with_lineage(std::sync::Arc::new(redacted));
        for (name, original, file) in pairs {
            let (expected, _) = originals.parse_all(&original).unwrap();
            let (actual, _) = redacted.parse_all(&file).unwrap();
            assert_eq!(
                shape(&actual.batch.records),
                shape(&expected.batch.records),
                "{name}"
            );
            assert_eq!(actual.stats.bad_lines, expected.stats.bad_lines, "{name}");
            assert_eq!(
                actual.stats.unknown_types, expected.stats.unknown_types,
                "{name}"
            );
        }
    }

    /// 把真实日志脱敏成 fixture（见模块说明）。默认不运行。
    #[test]
    #[ignore = "run by hand with LECTERN_REDACT_IN and LECTERN_REDACT_OUT"]
    fn redact_file_from_env() {
        let input = std::env::var("LECTERN_REDACT_IN").expect("LECTERN_REDACT_IN");
        let output = std::env::var("LECTERN_REDACT_OUT").expect("LECTERN_REDACT_OUT");
        let mut r = Redactor::default();
        let text = std::fs::read_to_string(input).unwrap();
        let out: String = text.lines().map(|l| r.line(l) + "\n").collect();
        std::fs::write(output, out).unwrap();
    }
}
