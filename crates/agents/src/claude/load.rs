//! 按引用读正文（docs/design/agents.md「轮次索引」：`load` 按偏移定位、读一行并解析）。

use std::borrow::Cow;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use lectern_services::session::{Content, ContentRef, EditOp, FileEdit, SessionError};
use serde::Deserialize;
use serde::de::value::MapAccessDeserializer;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

use super::format::{Lenient, StrOrSeq};
use super::io_error;
use super::parse::PART_TEXT;
use super::tools::{self, Class, absolute};

#[derive(Deserialize)]
struct LoadLine<'a> {
    #[serde(borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(borrow)]
    message: Option<LoadMessage<'a>>,
    #[serde(rename = "toolUseResult", borrow, default)]
    tool_use_result: MaybeEdit<'a>,
}

#[derive(Deserialize)]
struct LoadMessage<'a> {
    #[serde(borrow)]
    content: Option<StrOrSeq<Cow<'a, str>, LoadBlock<'a>>>,
}

#[derive(Deserialize)]
struct LoadBlock<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
    is_error: Option<bool>,
    #[serde(borrow)]
    content: Option<StrOrSeq<Cow<'a, str>, LoadResultBlock<'a>>>,
}

#[derive(Deserialize)]
struct LoadResultBlock<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

/// `Write`、`Edit`、`MultiEdit`、`NotebookEdit` 的输入。
#[derive(Deserialize)]
struct EditInput<'a> {
    #[serde(borrow)]
    file_path: Option<Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<Cow<'a, str>>,
    #[serde(borrow)]
    old_string: Option<Cow<'a, str>>,
    #[serde(borrow)]
    new_string: Option<Cow<'a, str>>,
    replace_all: Option<bool>,
    #[serde(borrow)]
    edits: Option<Vec<EditPair<'a>>>,
}

#[derive(Deserialize)]
struct EditPair<'a> {
    #[serde(borrow)]
    old_string: Cow<'a, str>,
    #[serde(borrow)]
    new_string: Cow<'a, str>,
    replace_all: Option<bool>,
}

/// 顶层 `toolUseResult` 里的改动；不是对象时没有。
#[derive(Default)]
enum MaybeEdit<'a> {
    Object(EditResult<'a>),
    #[default]
    None,
}

/// 各种工具的 `toolUseResult` 都会进到这里（`Task` 的 `content` 是数组），所以字段的类型不对时当作没有。
#[derive(Deserialize)]
struct EditResult<'a> {
    #[serde(rename = "filePath", borrow, default)]
    file_path: Lenient<'a>,
    #[serde(rename = "type", borrow, default)]
    kind: Lenient<'a>,
    #[serde(borrow, default)]
    content: Lenient<'a>,
    #[serde(rename = "originalFile", default)]
    original_file: Presence,
    #[serde(rename = "structuredPatch", borrow)]
    structured_patch: Option<Vec<Hunk<'a>>>,
}

#[derive(Deserialize)]
struct Hunk<'a> {
    #[serde(rename = "oldStart")]
    old_start: u64,
    #[serde(rename = "oldLines")]
    old_lines: u64,
    #[serde(rename = "newStart")]
    new_start: u64,
    #[serde(rename = "newLines")]
    new_lines: u64,
    #[serde(borrow)]
    lines: Vec<Cow<'a, str>>,
}

impl<'de: 'a, 'a> Deserialize<'de> for MaybeEdit<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(std::marker::PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = MaybeEdit<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a tool result")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                EditResult::deserialize(MapAccessDeserializer::new(map)).map(MaybeEdit::Object)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(MaybeEdit::None)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(MaybeEdit::None)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(MaybeEdit::None)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(MaybeEdit::None)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(MaybeEdit::None)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(MaybeEdit::None)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(MaybeEdit::None)
            }
        }
        d.deserialize_any(V(std::marker::PhantomData))
    }
}

/// 区分字段缺失、为 null 和有值。
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Presence {
    #[default]
    Missing,
    Null,
    Present,
}

impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(PresenceVisitor)
    }
}

struct PresenceVisitor;

impl<'de> Visitor<'de> for PresenceVisitor {
    type Value = Presence;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Presence, E> {
        Ok(Presence::Null)
    }
    fn visit_none<E: de::Error>(self) -> Result<Presence, E> {
        Ok(Presence::Null)
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Presence, E> {
        Ok(Presence::Present)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Presence, E> {
        Ok(Presence::Present)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Presence, E> {
        Ok(Presence::Present)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Presence, E> {
        Ok(Presence::Present)
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Presence, E> {
        Ok(Presence::Present)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Presence, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Presence::Present)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Presence, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Presence::Present)
    }
}

/// 引用对不上文件（文件被改写过）。
fn stale() -> SessionError {
    SessionError::Io("the content reference does not match the session file".into())
}

pub(super) fn load(path: &Path, at: &ContentRef) -> Result<Content, SessionError> {
    let mut f = File::open(path).map_err(io_error)?;
    f.seek(SeekFrom::Start(at.pos())).map_err(io_error)?;
    let mut buf = vec![0; at.line_len() as usize + 1];
    match f.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(stale()),
        Err(e) => return Err(io_error(e)),
    }
    if buf.pop() != Some(b'\n') {
        return Err(stale());
    }
    let line: LoadLine = serde_json::from_slice(&buf).map_err(|_| stale())?;
    let cwd = Path::new(line.cwd.as_deref().unwrap_or("/"));
    let content = line.message.and_then(|m| m.content);
    if at.part() == PART_TEXT {
        return Ok(Content::Text(match content {
            Some(StrOrSeq::Str(s)) => s.into(),
            Some(StrOrSeq::Seq(blocks)) => join(
                blocks
                    .iter()
                    .filter(|b| b.kind.as_deref() == Some("text"))
                    .filter_map(|b| b.text.as_deref()),
            )
            .into(),
            None => "".into(),
        }));
    }
    let Some(StrOrSeq::Seq(blocks)) = content else {
        return Err(stale());
    };
    let Some(block) = blocks.get(usize::from(at.part())) else {
        return Err(stale());
    };
    match block.kind.as_deref() {
        Some("text") => Ok(Content::Text(
            block.text.as_deref().unwrap_or_default().into(),
        )),
        Some("tool_use") => Ok(tool_input(
            block.name.as_deref().unwrap_or_default(),
            block.input,
            cwd,
        )),
        Some("tool_result") => Ok(tool_result(block, &line.tool_use_result, cwd)),
        _ => Err(stale()),
    }
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

/// 工具输入：`Write`、`Edit`、`MultiEdit` 给出改动，其余给出原样的 JSON。
fn tool_input(name: &str, input: Option<&RawValue>, cwd: &Path) -> Content {
    let raw = input.map_or("{}", RawValue::get);
    let edits = (tools::class(name) == Class::EditFile)
        .then(|| serde_json::from_str::<EditInput>(raw).ok())
        .flatten()
        .and_then(|i| {
            let path = absolute(cwd, i.file_path.as_deref()?);
            let replace = |old: &str, new: &str, all: Option<bool>| FileEdit {
                path: path.clone(),
                op: EditOp::Replace {
                    old: old.into(),
                    new: new.into(),
                    all: all == Some(true),
                },
            };
            let edits: Vec<FileEdit> = match name {
                // 输入里看不出是不是新建；结果里的 `originalFile` 才说得清（`review` 优先用结果）。
                "Write" => vec![FileEdit {
                    path: path.clone(),
                    op: EditOp::Write {
                        content: i.content.as_deref()?.into(),
                        created: false,
                    },
                }],
                "Edit" => vec![replace(
                    i.old_string.as_deref()?,
                    i.new_string.as_deref()?,
                    i.replace_all,
                )],
                _ => i
                    .edits?
                    .iter()
                    .map(|e| replace(&e.old_string, &e.new_string, e.replace_all))
                    .collect(),
            };
            Some(edits)
        });
    match edits {
        Some(edits) => Content::Edits(edits.into()),
        None => Content::Text(raw.into()),
    }
}

/// 工具结果：成功的 `Write`、`Edit`、`MultiEdit` 给出 `toolUseResult` 里的改动，其余给出结果文本。
fn tool_result(block: &LoadBlock<'_>, use_result: &MaybeEdit<'_>, cwd: &Path) -> Content {
    let text = || -> Content {
        Content::Text(
            match &block.content {
                Some(StrOrSeq::Str(s)) => s.to_string(),
                Some(StrOrSeq::Seq(parts)) => join(
                    parts
                        .iter()
                        .filter(|p| p.kind.as_deref() == Some("text"))
                        .filter_map(|p| p.text.as_deref()),
                ),
                None => String::new(),
            }
            .into(),
        )
    };
    let MaybeEdit::Object(r) = use_result else {
        return text();
    };
    let Some(path) = r.file_path.0.as_deref() else {
        return text();
    };
    if block.is_error == Some(true) {
        return text();
    }
    let path = absolute(cwd, path);
    let op = match (r.kind.0.as_deref(), &r.content.0, &r.structured_patch) {
        (Some(kind @ ("create" | "update")), Some(content), _) => EditOp::Write {
            content: content.as_ref().into(),
            created: match r.original_file {
                Presence::Null => true,
                Presence::Present => false,
                Presence::Missing => kind == "create",
            },
        },
        (_, _, Some(hunks)) if !hunks.is_empty() => EditOp::Patch {
            hunks: render(hunks).into(),
            move_to: None,
        },
        _ => return text(),
    };
    Content::Edits(vec![FileEdit { path, op }].into())
}

/// `structuredPatch` 到统一 diff 的 hunk 文本（不含文件头）。
fn render(hunks: &[Hunk<'_>]) -> String {
    let mut out = String::new();
    for h in hunks {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            h.old_start, h.old_lines, h.new_start, h.new_lines
        ));
        for line in &h.lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
