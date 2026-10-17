//! 按引用读正文（docs/design/agents.md「轮次索引」：`load` 按偏移定位、读一行并解析）。
//!
//! `ContentRef::part` 在 Codex 里说的是这一行的哪一部分（[`PART_TEXT`]、[`PART_INPUT`]、[`PART_OUTPUT`]、
//! [`PART_CHANGES`]），一行只有一个条目。补丁输入里的相对路径要按调用时的 cwd 拼接：向前最多 64 行找最近的
//! `turn_context`，找不到时取 `session_meta` 的 cwd。

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::session::{Content, ContentRef, EditOp, FileEdit, SessionError};
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

use super::Body;
use super::parse::{PART_CHANGES, PART_INPUT, PART_OUTPUT, PART_TEXT};
use super::tools::{self, PatchOp};
use crate::lines::{Lines, Next};
use crate::path::absolute;
use crate::read::io_error;
use crate::tail::scan_back;

#[derive(Deserialize)]
struct LoadLine<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    payload: Option<&'a RawValue>,
}

/// `event_msg` 的 `payload`：`user_message` / `agent_message`、`patch_apply_end`、`item_completed`。
#[derive(Deserialize)]
struct LoadEvent<'a> {
    #[serde(borrow)]
    message: Option<Cow<'a, str>>,
    #[serde(borrow)]
    changes: Option<BTreeMap<Cow<'a, str>, Change<'a>>>,
    #[serde(borrow)]
    item: Option<LoadItem<'a>>,
}

#[derive(Deserialize)]
struct LoadItem<'a> {
    #[serde(borrow)]
    content: Option<Vec<LoadPart<'a>>>,
    #[serde(borrow)]
    changes: Option<BTreeMap<Cow<'a, str>, Change<'a>>>,
}

#[derive(Deserialize)]
struct LoadPart<'a> {
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

/// `response_item` 的 `payload`：工具调用与输出。
#[derive(Deserialize)]
struct LoadResponse<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    arguments: Option<Cow<'a, str>>,
    #[serde(borrow)]
    input: Option<Cow<'a, str>>,
    #[serde(borrow)]
    action: Option<&'a RawValue>,
    #[serde(borrow)]
    output: Option<LoadOutput<'a>>,
}

/// `compacted` 的 `payload`。
#[derive(Deserialize)]
struct LoadCompacted<'a> {
    #[serde(borrow)]
    message: Option<Cow<'a, str>>,
}

/// `FileChange`：`{"type": "add" | "delete" | "update", …}`；旧版本是外部标记（`{"update": {…}}`）。
#[derive(Deserialize)]
#[serde(untagged)]
enum Change<'a> {
    #[serde(borrow)]
    Tagged(Tagged<'a>),
    #[serde(borrow)]
    External(External<'a>),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Tagged<'a> {
    Add {
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
    Delete {},
    Update {
        #[serde(borrow)]
        unified_diff: Cow<'a, str>,
        #[serde(borrow)]
        move_path: Option<Cow<'a, str>>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum External<'a> {
    Add {
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
    Delete {},
    Update {
        #[serde(borrow)]
        unified_diff: Cow<'a, str>,
        #[serde(borrow)]
        move_path: Option<Cow<'a, str>>,
    },
}

/// 工具输出的全文：字符串、内容项（`input_text` 用换行拼接），或旧版本的 `{content, success}`。
struct LoadOutput<'a>(Cow<'a, str>);

impl<'de: 'a, 'a> Deserialize<'de> for LoadOutput<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Old<'a> {
            #[serde(borrow)]
            content: Option<LoadOutput<'a>>,
        }
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = LoadOutput<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a tool output")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(LoadOutput(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(LoadOutput(Cow::Owned(v.to_owned())))
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut parts = Vec::new();
                while let Some(part) = seq.next_element::<LoadPart<'de>>()? {
                    parts.extend(part.text);
                }
                Ok(LoadOutput(Cow::Owned(join(parts.iter().map(|p| &**p)))))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let old = Old::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(old.content.unwrap_or(LoadOutput(Cow::Borrowed(""))))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LoadOutput(Cow::Borrowed("")))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// `apply_patch` 作为 `function_call` 时的参数。
#[derive(Deserialize)]
struct PatchArgs<'a> {
    #[serde(borrow)]
    input: Option<Cow<'a, str>>,
}

/// 只读 cwd 的行（`session_meta`、`turn_context`）。
#[derive(Deserialize)]
struct CwdLine<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    payload: Option<CwdPayload<'a>>,
}

#[derive(Deserialize)]
struct CwdPayload<'a> {
    #[serde(borrow)]
    cwd: Option<Cow<'a, str>>,
}

/// 引用对不上文件（文件被改写过）。
fn stale() -> SessionError {
    SessionError::Io("the content reference does not match the session file".into())
}

fn join<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for (i, p) in parts.enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(p);
    }
    out
}

/// 一行里的 cwd（只认 `session_meta`、`turn_context`）。
fn cwd_of(bytes: &[u8]) -> Option<Arc<Path>> {
    let line: CwdLine = serde_json::from_slice(bytes).ok()?;
    if !matches!(line.kind.as_deref(), Some("session_meta" | "turn_context")) {
        return None;
    }
    Some(Path::new(&*line.payload?.cwd?).into())
}

/// 调用时的 cwd 从哪里来：已经从头读到这一行（流式解码时）时直接给出，否则在文件里找。
enum CwdSource<'b> {
    Known(Option<Arc<Path>>),
    File(&'b mut std::fs::File, u64),
}

impl CwdSource<'_> {
    fn resolve(self) -> Option<Arc<Path>> {
        match self {
            CwdSource::Known(cwd) => cwd,
            CwdSource::File(f, pos) => {
                let mut found = None;
                let _ = scan_back(f, pos, |bytes| {
                    if !bytes.windows(14).any(|w| w == b"\"turn_context\"") {
                        return false;
                    }
                    found = cwd_of(bytes);
                    found.is_some()
                });
                if found.is_some() {
                    return found;
                }
                // 第一行是 `session_meta`。
                f.seek(SeekFrom::Start(0)).ok()?;
                let mut lines = Lines::new(&mut *f, 0, b"");
                match lines.next().ok()? {
                    Next::Line(_) => cwd_of(lines.line()),
                    _ => None,
                }
            }
        }
    }
}

pub(super) fn load(body: Body<'_>, at: &ContentRef) -> Result<Content, SessionError> {
    let want = at.line_len() as usize + 1;
    match body {
        Body::File(mut f, _guard) => {
            f.seek(SeekFrom::Start(at.pos())).map_err(io_error)?;
            let mut buf = vec![0; want];
            match f.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(stale()),
                Err(e) => return Err(io_error(e)),
            }
            if buf.pop() != Some(b'\n') {
                return Err(stale());
            }
            content(&buf, at, CwdSource::File(&mut f, at.pos()))
        }
        Body::Stream(r) => {
            // 从头解码到这一行，顺带记下最近的 cwd。
            let mut lines = Lines::new(r, 0, b"");
            let mut cwd = None;
            loop {
                match lines.next().map_err(io_error)? {
                    Next::End => return Err(stale()),
                    Next::Oversized(pos) if pos >= at.pos() => return Err(stale()),
                    Next::Oversized(_) => {}
                    Next::Line(pos) if pos == at.pos() => {
                        if lines.line().len() + 1 != want {
                            return Err(stale());
                        }
                        let line = lines.line().to_vec();
                        return content(&line, at, CwdSource::Known(cwd));
                    }
                    Next::Line(pos) if pos > at.pos() => return Err(stale()),
                    Next::Line(_) => {
                        if let Some(c) = cwd_of(lines.line()) {
                            cwd = Some(c);
                        }
                    }
                }
            }
        }
    }
}

fn content(bytes: &[u8], at: &ContentRef, cwd: CwdSource<'_>) -> Result<Content, SessionError> {
    let line: LoadLine = serde_json::from_slice(bytes).map_err(|_| stale())?;
    let payload = line.payload.ok_or_else(stale)?.get();
    let kind = line.kind.as_deref().unwrap_or_default();
    match (kind, at.part()) {
        ("event_msg", PART_TEXT) => {
            let e: LoadEvent = serde_json::from_str(payload).map_err(|_| stale())?;
            if let Some(m) = e.message {
                return Ok(Content::Text(m.as_ref().into()));
            }
            let parts = e.item.and_then(|i| i.content).ok_or_else(stale)?;
            Ok(Content::Text(
                join(parts.iter().filter_map(|p| p.text.as_deref())).into(),
            ))
        }
        ("compacted", PART_TEXT) => {
            let c: LoadCompacted = serde_json::from_str(payload).map_err(|_| stale())?;
            Ok(Content::Text(c.message.unwrap_or_default().as_ref().into()))
        }
        ("event_msg", PART_CHANGES) => {
            let e: LoadEvent = serde_json::from_str(payload).map_err(|_| stale())?;
            let changes = e
                .changes
                .or_else(|| e.item.and_then(|i| i.changes))
                .ok_or_else(stale)?;
            Ok(Content::Edits(file_changes(changes, cwd).into()))
        }
        ("response_item", PART_INPUT) => {
            let r: LoadResponse = serde_json::from_str(payload).map_err(|_| stale())?;
            let name = r.name.as_deref().unwrap_or_default();
            match r.kind.as_deref() {
                Some("function_call") => {
                    let args = r.arguments.unwrap_or_default();
                    if tools::class(name) == tools::Class::Patch
                        && let Ok(PatchArgs { input: Some(patch) }) = serde_json::from_str(&args)
                    {
                        return Ok(Content::Edits(patch_edits(&patch, cwd).into()));
                    }
                    Ok(Content::Text(args.as_ref().into()))
                }
                Some("custom_tool_call") => {
                    let input = r.input.unwrap_or_default();
                    if tools::class(name) == tools::Class::Patch {
                        return Ok(Content::Edits(patch_edits(&input, cwd).into()));
                    }
                    Ok(Content::Text(input.as_ref().into()))
                }
                Some("local_shell_call") => {
                    Ok(Content::Text(r.action.map_or("{}", RawValue::get).into()))
                }
                _ => Err(stale()),
            }
        }
        ("response_item", PART_OUTPUT) => {
            let r: LoadResponse = serde_json::from_str(payload).map_err(|_| stale())?;
            Ok(Content::Text(r.output.ok_or_else(stale)?.0.as_ref().into()))
        }
        _ => Err(stale()),
    }
}

/// 只在需要（有相对路径）时才找 cwd。
struct Cwd<'b> {
    source: Option<CwdSource<'b>>,
    value: Option<Arc<Path>>,
}

impl<'b> Cwd<'b> {
    fn new(source: CwdSource<'b>) -> Self {
        Cwd {
            source: Some(source),
            value: None,
        }
    }

    fn path(&mut self, p: &str) -> PathBuf {
        if Path::new(p).is_absolute() {
            return absolute(Path::new("/"), p);
        }
        if let Some(source) = self.source.take() {
            self.value = source.resolve();
        }
        match &self.value {
            Some(cwd) => absolute(cwd, p),
            // 找不到 cwd：原样交出相对路径。
            None => PathBuf::from(p),
        }
    }
}

fn patch_edits(patch: &str, cwd: CwdSource<'_>) -> Vec<FileEdit> {
    let mut cwd = Cwd::new(cwd);
    let lines = |body: &[&str]| -> Arc<str> {
        let mut out = String::new();
        for l in body {
            out.push_str(l);
            out.push('\n');
        }
        out.into()
    };
    tools::patch_files(patch)
        .into_iter()
        .map(|f| FileEdit {
            path: cwd.path(f.path),
            op: match f.op {
                PatchOp::Add => EditOp::Add {
                    content: lines(&f.body),
                },
                PatchOp::Delete => EditOp::Delete,
                PatchOp::Update => EditOp::Patch {
                    hunks: lines(&f.body),
                    move_to: f.move_to.map(|m| cwd.path(m)),
                },
            },
        })
        .collect()
}

fn file_changes(changes: BTreeMap<Cow<'_, str>, Change<'_>>, cwd: CwdSource<'_>) -> Vec<FileEdit> {
    let mut cwd = Cwd::new(cwd);
    changes
        .into_iter()
        .map(|(path, change)| {
            let tagged = match change {
                Change::Tagged(t) => t,
                Change::External(External::Add { content }) => Tagged::Add { content },
                Change::External(External::Delete {}) => Tagged::Delete {},
                Change::External(External::Update {
                    unified_diff,
                    move_path,
                }) => Tagged::Update {
                    unified_diff,
                    move_path,
                },
            };
            let op = match tagged {
                Tagged::Add { content } => EditOp::Add {
                    content: content.as_ref().into(),
                },
                Tagged::Delete {} => EditOp::Delete,
                Tagged::Update {
                    unified_diff,
                    move_path,
                } => EditOp::Patch {
                    hunks: unified_diff.as_ref().into(),
                    move_to: move_path.map(|m| cwd.path(&m)),
                },
            };
            FileEdit {
                path: cwd.path(&path),
                op,
            }
        })
        .collect()
}
