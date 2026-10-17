//! Claude Code 会话行的形状（docs/design/agents.md「外部格式」）。
//!
//! 只声明用到的字段，其余忽略；字符串尽量借用原行。读取路径上的文本只留开头（[`TextHead`]），
//! 工具输入留原样 JSON（[`RawValue`]），只有分类表里的工具才解析；正文在 `load` 里另行解析。

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

use serde::Deserialize;
use serde::de::value::{BorrowedStrDeserializer, MapAccessDeserializer, StrDeserializer};
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

/// 一行记录。
#[derive(Deserialize)]
pub(super) struct Line<'a> {
    #[serde(rename = "type", borrow)]
    pub kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub uuid: Option<Cow<'a, str>>,
    #[serde(rename = "sessionId", borrow)]
    pub session_id: Option<Cow<'a, str>>,
    #[serde(rename = "agentId", borrow)]
    pub agent_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub cwd: Option<Cow<'a, str>>,
    #[serde(rename = "gitBranch", borrow)]
    pub git_branch: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub version: Option<Cow<'a, str>>,
    #[serde(rename = "isSidechain")]
    pub is_sidechain: Option<bool>,
    #[serde(rename = "isMeta")]
    pub is_meta: Option<bool>,
    #[serde(rename = "isCompactSummary")]
    pub is_compact_summary: Option<bool>,
    #[serde(rename = "requestId", borrow)]
    pub request_id: Option<Cow<'a, str>>,
    #[serde(rename = "advisorModel", borrow)]
    pub advisor_model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub origin: Option<Origin<'a>>,
    #[serde(borrow)]
    pub message: Option<Message<'a>>,
    #[serde(rename = "toolUseResult", borrow)]
    pub tool_use_result: Option<UseResult<'a>>,
}

#[derive(Deserialize)]
pub(super) struct Origin<'a> {
    #[serde(borrow)]
    pub kind: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
pub(super) struct Message<'a> {
    #[serde(borrow)]
    pub id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub stop_reason: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub usage: Option<Usage<'a>>,
    #[serde(borrow)]
    pub content: Option<StrOrSeq<TextHead<'a>, Block<'a>>>,
}

/// 内容块：`text`、`tool_use`、`tool_result`、`thinking`（不读）、`image` 等。
#[derive(Deserialize)]
pub(super) struct Block<'a> {
    #[serde(rename = "type", borrow)]
    pub kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub text: Option<TextHead<'a>>,
    #[serde(borrow)]
    pub id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub input: Option<&'a RawValue>,
    #[serde(borrow)]
    pub tool_use_id: Option<Cow<'a, str>>,
    pub is_error: Option<bool>,
    #[serde(borrow)]
    pub content: Option<StrOrSeq<TextHead<'a>, ResultBlock<'a>>>,
}

#[derive(Deserialize)]
pub(super) struct ResultBlock<'a> {
    #[serde(rename = "type", borrow)]
    pub kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub text: Option<TextHead<'a>>,
}

#[derive(Deserialize)]
pub(super) struct Usage<'a> {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_creation: Option<CacheCreation>,
    #[serde(borrow)]
    pub iterations: Option<Vec<Iteration<'a>>>,
}

#[derive(Deserialize)]
pub(super) struct CacheCreation {
    pub ephemeral_5m_input_tokens: Option<u64>,
    pub ephemeral_1h_input_tokens: Option<u64>,
}

/// `usage.iterations[]` 的一项；`type: "advisor_message"` 是 advisor 的用量。
#[derive(Deserialize)]
pub(super) struct Iteration<'a> {
    #[serde(rename = "type", borrow)]
    pub kind: Option<Cow<'a, str>>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_creation: Option<CacheCreation>,
    #[serde(rename = "advisorModel", borrow)]
    pub advisor_model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub model: Option<Cow<'a, str>>,
}

/// 顶层 `toolUseResult`：对象（读 `interrupted`）、字符串（错误或拒绝的说明）或别的。
pub(super) enum UseResult<'a> {
    Object { interrupted: bool },
    Text(TextHead<'a>),
    Other,
}

#[derive(Deserialize)]
struct UseResultObject {
    interrupted: Option<bool>,
}

impl<'de: 'a, 'a> Deserialize<'de> for UseResult<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = UseResult<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a tool result")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let o = UseResultObject::deserialize(MapAccessDeserializer::new(map))?;
                Ok(UseResult::Object {
                    interrupted: o.interrupted == Some(true),
                })
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(UseResult::Text(TextHead::new(Cow::Borrowed(v))))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(UseResult::Text(TextHead::owned(v)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(UseResult::Other)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(UseResult::Other)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(UseResult::Other)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(UseResult::Other)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(UseResult::Other)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UseResult::Other)
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// 读取路径上只需要文本的开头：prompt 的预览、包装标记的判断、是否为空。
pub(super) struct TextHead<'a> {
    /// 去掉开头空白之后的最多 [`TextHead::MAX`] 字节。
    pub head: Cow<'a, str>,
    pub blank: bool,
}

impl<'a> TextHead<'a> {
    const MAX: usize = 2048;

    fn new(s: Cow<'a, str>) -> Self {
        let blank = s.trim().is_empty();
        let head = match s {
            Cow::Borrowed(s) => Cow::Borrowed(cut(s.trim_start())),
            Cow::Owned(s) => Cow::Owned(cut(s.trim_start()).to_owned()),
        };
        TextHead { head, blank }
    }

    fn owned(s: &str) -> Self {
        TextHead {
            head: Cow::Owned(cut(s.trim_start()).to_owned()),
            blank: s.trim().is_empty(),
        }
    }
}

fn cut(s: &str) -> &str {
    if s.len() <= TextHead::MAX {
        return s;
    }
    let mut end = TextHead::MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl<'de: 'a, 'a> Deserialize<'de> for TextHead<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = TextHead<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(TextHead::new(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(TextHead::owned(v))
            }
        }
        d.deserialize_str(V(PhantomData))
    }
}

/// 字符串或数组：`message.content`、`tool_result.content`。`null` 当作空数组。
pub(super) enum StrOrSeq<S, B> {
    Str(S),
    Seq(Vec<B>),
}

impl<'de, S: Deserialize<'de>, B: Deserialize<'de>> Deserialize<'de> for StrOrSeq<S, B> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<S, B>(PhantomData<(S, B)>);
        impl<'de, S: Deserialize<'de>, B: Deserialize<'de>> Visitor<'de> for V<S, B> {
            type Value = StrOrSeq<S, B>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or an array")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                S::deserialize(BorrowedStrDeserializer::<E>::new(v)).map(StrOrSeq::Str)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                S::deserialize(StrDeserializer::<E>::new(v)).map(StrOrSeq::Str)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(1).min(64));
                while let Some(b) = seq.next_element()? {
                    out.push(b);
                }
                Ok(StrOrSeq::Seq(out))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrOrSeq::Seq(Vec::new()))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// 工具输入里分类要用的字段。值的类型不对（MCP 工具的任意输入）时当作没有。
#[derive(Deserialize, Default)]
pub(super) struct ToolInput<'a> {
    #[serde(borrow, default)]
    pub file_path: Lenient<'a>,
    #[serde(borrow, default)]
    pub notebook_path: Lenient<'a>,
    #[serde(borrow, default)]
    pub path: Lenient<'a>,
    #[serde(borrow, default)]
    pub command: Lenient<'a>,
}

/// 是字符串就留下，否则丢弃。
#[derive(Default)]
pub(super) struct Lenient<'a>(pub Option<Cow<'a, str>>);

impl<'de: 'a, 'a> Deserialize<'de> for Lenient<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Lenient<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(Lenient(Some(Cow::Borrowed(v))))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Lenient(Some(Cow::Owned(v.to_owned()))))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Lenient(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Lenient(None))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// `agent-<agentId>.meta.json`。
#[derive(Deserialize)]
pub(super) struct AgentMeta {
    #[serde(rename = "toolUseId")]
    pub tool_use_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_lines_tolerate_unknown_and_missing_fields() {
        let line: Line = serde_json::from_str(
            r#"{"type":"user","future":{"a":[1,2]},"message":{"content":"hi \"there\""}}"#,
        )
        .unwrap();
        assert_eq!(line.kind.as_deref(), Some("user"));
        assert!(line.uuid.is_none());
        let Some(StrOrSeq::Str(text)) = line.message.unwrap().content else {
            panic!("string content")
        };
        assert_eq!(text.head, "hi \"there\"");

        let block: Block = serde_json::from_str(
            r#"{"type":"tool_result","tool_use_id":"t","content":[{"type":"image","source":{}},{"type":"text","text":"  ok"}]}"#,
        )
        .unwrap();
        let Some(StrOrSeq::Seq(parts)) = block.content else {
            panic!("array content")
        };
        assert_eq!(parts[1].text.as_ref().unwrap().head, "ok");

        let input: ToolInput =
            serde_json::from_str(r#"{"path":{"nested":true},"command":"ls","x":[1]}"#).unwrap();
        assert!(input.path.0.is_none());
        assert_eq!(input.command.0.as_deref(), Some("ls"));
    }

    #[test]
    fn format_tool_use_result_shapes() {
        let r: UseResult = serde_json::from_str(r#"{"stdout":"x","interrupted":true}"#).unwrap();
        assert!(matches!(r, UseResult::Object { interrupted: true }));
        let r: UseResult = serde_json::from_str(r#""User rejected tool use""#).unwrap();
        assert!(matches!(r, UseResult::Text(t) if t.head == "User rejected tool use"));
        let r: UseResult = serde_json::from_str("[1, {}]").unwrap();
        assert!(matches!(r, UseResult::Other));
    }

    #[test]
    fn format_text_head_is_bounded() {
        let long = format!("\"{}\"", "a".repeat(10_000));
        let t: TextHead = serde_json::from_str(&long).unwrap();
        assert_eq!(t.head.len(), TextHead::MAX);
        let t: TextHead = serde_json::from_str(r#"" \n ""#).unwrap();
        assert!(t.blank);
    }
}
