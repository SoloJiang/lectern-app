//! Codex rollout 行的形状（docs/design/agents.md「外部格式」的 Codex 部分）。
//!
//! 每行 `{timestamp, ordinal?, type, payload}`。`payload` 按 `type` 直接反序列化到只声明用到字段的结构，
//! 不先缓存成通用的值树；`event_msg`、`response_item` 和 `item_completed` 的 `item` 是内部标记的对象，
//! Codex 总把 `type` 写在第一个键，所以读到标记之后按它选结构，其余类型的内容跳过（[`IgnoredAny`]）。
//! 正文（工具参数、输出、改动）在读取路径上只留开头和末尾（[`Text`]）或原样 JSON（[`RawValue`]），
//! `load` 另行解析。

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

use serde::Deserialize;
use serde::de::value::{BorrowedStrDeserializer, MapAccessDeserializer, StringDeserializer};
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

/// 一行记录。
pub(super) struct Line<'a> {
    pub timestamp: Option<Cow<'a, str>>,
    pub ordinal: Option<u64>,
    pub kind: Option<Cow<'a, str>>,
    pub payload: Payload<'a>,
}

/// 按 `type` 选出的 `payload`。
pub(super) enum Payload<'a> {
    SessionMeta(SessionMeta<'a>),
    TurnContext(TurnContext<'a>),
    Response(Response<'a>),
    Event(Event<'a>),
    Compacted,
    TokenUsageRecord(TokenUsageRecord<'a>),
    InterAgentMetadata {
        trigger_turn: bool,
    },
    /// 已知忽略或不认识的类型，或者没有 `payload`。
    Skipped,
}

impl<'de: 'a, 'a> Deserialize<'de> for Line<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Line<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a rollout line")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut line = Line {
                    timestamp: None,
                    ordinal: None,
                    kind: None,
                    payload: Payload::Skipped,
                };
                // `payload` 写在 `type` 之前时先留下原样 JSON，读完整行再解析。
                let mut early: Option<&'de RawValue> = None;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match &*key {
                        "timestamp" => line.timestamp = map.next_value()?,
                        "ordinal" => line.ordinal = map.next_value()?,
                        "type" => line.kind = map.next_value()?,
                        "payload" => match line.kind.as_deref() {
                            Some(kind) => line.payload = map.next_value_seed(PayloadSeed(kind))?,
                            None => early = Some(map.next_value()?),
                        },
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                if let (Some(raw), Some(kind)) = (early, line.kind.as_deref()) {
                    let mut de = serde_json::Deserializer::from_str(raw.get());
                    line.payload = PayloadSeed(kind)
                        .deserialize(&mut de)
                        .map_err(de::Error::custom)?;
                }
                Ok(line)
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}

struct PayloadSeed<'k>(&'k str);

impl<'de, 'k> DeserializeSeed<'de> for PayloadSeed<'k> {
    type Value = Payload<'de>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        Ok(match self.0 {
            "session_meta" => Payload::SessionMeta(SessionMeta::deserialize(d)?),
            "turn_context" => Payload::TurnContext(TurnContext::deserialize(d)?),
            "response_item" => Payload::Response(Response::deserialize(d)?),
            "event_msg" => Payload::Event(Event::deserialize(d)?),
            "compacted" => {
                IgnoredAny::deserialize(d)?;
                Payload::Compacted
            }
            "token_usage_record" => Payload::TokenUsageRecord(TokenUsageRecord::deserialize(d)?),
            "inter_agent_communication_metadata" => {
                #[derive(Deserialize)]
                struct M {
                    trigger_turn: Option<bool>,
                }
                Payload::InterAgentMetadata {
                    trigger_turn: M::deserialize(d)?.trigger_turn == Some(true),
                }
            }
            _ => {
                IgnoredAny::deserialize(d)?;
                Payload::Skipped
            }
        })
    }
}

/// `session_meta`（首行）。`base_instructions` 一类的大字段不声明，跳过。
#[derive(Deserialize)]
pub(super) struct SessionMeta<'a> {
    #[serde(borrow)]
    pub id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub cwd: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub cli_version: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    pub source: Source<'a>,
    #[serde(borrow)]
    pub parent_thread_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub forked_from_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub history_mode: Option<Cow<'a, str>>,
    pub subagent_history_start_ordinal: Option<u64>,
    #[serde(borrow)]
    pub git: Option<Git<'a>>,
}

#[derive(Deserialize)]
pub(super) struct Git<'a> {
    #[serde(borrow)]
    pub branch: Option<Cow<'a, str>>,
}

/// `session_meta.source`：`"cli"` 一类的字符串，或 `{"subagent": "review"}`、
/// `{"subagent": {"thread_spawn": {"parent_thread_id": …}}}` 一类的对象。
#[derive(Default)]
pub(super) struct Source<'a> {
    pub subagent: bool,
    pub parent_thread_id: Option<Cow<'a, str>>,
}

impl<'de: 'a, 'a> Deserialize<'de> for Source<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Source<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a session source")
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Source::default())
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Source::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Source::default();
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    if key == "subagent" {
                        out.subagent = true;
                        out.parent_thread_id = map.next_value::<SubAgent<'de>>()?.0;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(out)
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// `source.subagent` 的值：字符串（`review`、`compact`），或带 `thread_spawn.parent_thread_id` 的对象。
struct SubAgent<'a>(Option<Cow<'a, str>>);

impl<'de: 'a, 'a> Deserialize<'de> for SubAgent<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Spawn<'a> {
            #[serde(borrow)]
            parent_thread_id: Option<Cow<'a, str>>,
        }
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = SubAgent<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a subagent source")
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(SubAgent(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut parent = None;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    if key == "thread_spawn" {
                        parent = map.next_value::<Spawn<'de>>()?.parent_thread_id;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(SubAgent(parent))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// `turn_context`。
#[derive(Deserialize)]
pub(super) struct TurnContext<'a> {
    #[serde(borrow)]
    pub cwd: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub model: Option<Cow<'a, str>>,
}

/// 顶层 `token_usage_record`（每个完成的响应一条）。
#[derive(Deserialize)]
pub(super) struct TokenUsageRecord<'a> {
    #[serde(borrow)]
    pub thread_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub turn_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub response_id: Option<Cow<'a, str>>,
    pub usage: Option<TokenUsage>,
}

/// Codex `TokenUsage`：`input_tokens` 含 `cached_input_tokens`，`output_tokens` 含 `reasoning_output_tokens`。
#[derive(Deserialize, Clone, Copy)]
pub(super) struct TokenUsage {
    pub input_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub cache_write_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
}

/// `token_count` 的 `info`。
#[derive(Deserialize)]
pub(super) struct TokenInfo {
    pub total_token_usage: Option<TokenUsage>,
    pub last_token_usage: Option<TokenUsage>,
}

/// `token_count` 的 `rate_limits`。
#[derive(Deserialize)]
pub(super) struct RateLimits<'a> {
    #[serde(borrow)]
    pub limit_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub plan_type: Option<Cow<'a, str>>,
    pub primary: Option<Window>,
    pub secondary: Option<Window>,
}

#[derive(Deserialize)]
pub(super) struct Window {
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    /// Unix 秒。
    pub resets_at: Option<i64>,
    /// 旧版本：相对记录时间的秒数。
    pub resets_in_seconds: Option<i64>,
}

/// 内部标记的对象：读到 `type` 之后按它的值选结构读其余的键。Codex 总把 `type` 写在第一个键，这时不缓存；
/// 不在第一个键时（文件被别的工具重写过）先留下其余键值的原样 JSON，找到标记后再读。没有 `type` 时为 `untagged`。
fn tagged<'de, A, T>(
    mut map: A,
    pick: impl FnOnce(Cow<'de, str>, Rest<'de, A>) -> Result<T, A::Error>,
    untagged: T,
) -> Result<T, A::Error>
where
    A: MapAccess<'de>,
{
    let Some(first) = map.next_key::<Cow<'de, str>>()? else {
        return Ok(untagged);
    };
    if first == "type" {
        let kind: Cow<'de, str> = map.next_value()?;
        return pick(kind, Rest::Live(map));
    }
    let mut entries = Vec::new();
    let mut kind = None;
    let mut key = Some(first);
    while let Some(k) = key {
        let v: &'de RawValue = map.next_value()?;
        if k == "type" && kind.is_none() {
            kind = Some(v);
        } else {
            entries.push((k, v));
        }
        key = map.next_key()?;
    }
    let Some(kind) = kind else {
        return Ok(untagged);
    };
    let kind = Cow::<'de, str>::deserialize(kind).map_err(de::Error::custom)?;
    pick(kind, Rest::Buffered(entries.into_iter(), None))
}

/// [`tagged`] 读完标记之后剩下的键值：直接接着读，或者从缓存的原样 JSON 读。
enum Rest<'de, A> {
    Live(A),
    Buffered(
        std::vec::IntoIter<(Cow<'de, str>, &'de RawValue)>,
        Option<&'de RawValue>,
    ),
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for Rest<'de, A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        match self {
            Rest::Live(map) => map.next_key_seed(seed),
            Rest::Buffered(entries, pending) => match entries.next() {
                Some((key, value)) => {
                    *pending = Some(value);
                    match key {
                        Cow::Borrowed(k) => seed.deserialize(BorrowedStrDeserializer::new(k)),
                        Cow::Owned(k) => seed.deserialize(StringDeserializer::new(k)),
                    }
                    .map(Some)
                }
                None => Ok(None),
            },
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        match self {
            Rest::Live(map) => map.next_value_seed(seed),
            Rest::Buffered(_, pending) => {
                let raw = pending
                    .take()
                    .ok_or_else(|| de::Error::custom("a value without a key"))?;
                seed.deserialize(raw).map_err(de::Error::custom)
            }
        }
    }
}

fn rest<'de, A: MapAccess<'de>, T: Deserialize<'de>>(map: A) -> Result<T, A::Error> {
    T::deserialize(MapAccessDeserializer::new(map))
}

fn skip<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

/// `event_msg` 的 `payload`。
#[expect(
    clippy::large_enum_variant,
    reason = "one short-lived value per parsed line; boxing would allocate on every token_count"
)]
pub(super) enum Event<'a> {
    /// `task_started`（别名 `turn_started`）。
    TurnStarted {
        turn_id: Option<Cow<'a, str>>,
    },
    /// `task_complete`（`turn_complete`）或 `turn_aborted`。
    TurnEnded {
        turn_id: Option<Cow<'a, str>>,
        aborted: bool,
    },
    /// `legacy` 模式的 prompt。
    UserMessage {
        message: Text<'a>,
    },
    /// `legacy` 模式的 agent 说明。
    AgentMessage {
        message: Text<'a>,
    },
    TokenCount {
        info: Option<TokenInfo>,
        rate_limits: Option<RateLimits<'a>>,
    },
    /// `legacy` 模式的改动结果。`changes` 只在 `load` 里读。
    PatchApplyEnd {
        call_id: Option<Cow<'a, str>>,
        status: Option<Cow<'a, str>>,
        success: Option<bool>,
    },
    /// `paginated` 模式的条目。
    ItemCompleted {
        item: Item<'a>,
    },
    Other(Cow<'a, str>),
    Untagged,
}

impl<'de: 'a, 'a> Deserialize<'de> for Event<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Turn<'a> {
            #[serde(borrow)]
            turn_id: Option<Cow<'a, str>>,
        }
        #[derive(Deserialize)]
        struct Message<'a> {
            #[serde(borrow)]
            message: Option<Text<'a>>,
        }
        #[derive(Deserialize)]
        struct Count<'a> {
            info: Option<TokenInfo>,
            #[serde(borrow)]
            rate_limits: Option<RateLimits<'a>>,
        }
        #[derive(Deserialize)]
        struct Patch<'a> {
            #[serde(borrow)]
            call_id: Option<Cow<'a, str>>,
            #[serde(borrow)]
            status: Option<Cow<'a, str>>,
            success: Option<bool>,
        }
        #[derive(Deserialize)]
        struct Completed<'a> {
            #[serde(borrow)]
            item: Option<Item<'a>>,
        }
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Event<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an event")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                tagged(
                    map,
                    |kind, map| {
                        Ok(match &*kind {
                            "task_started" | "turn_started" => Event::TurnStarted {
                                turn_id: rest::<_, Turn>(map)?.turn_id,
                            },
                            "task_complete" | "turn_complete" | "turn_aborted" => {
                                Event::TurnEnded {
                                    turn_id: rest::<_, Turn>(map)?.turn_id,
                                    aborted: kind == "turn_aborted",
                                }
                            }
                            "user_message" => Event::UserMessage {
                                message: rest::<_, Message>(map)?.message.unwrap_or_default(),
                            },
                            "agent_message" => Event::AgentMessage {
                                message: rest::<_, Message>(map)?.message.unwrap_or_default(),
                            },
                            "token_count" => {
                                let c: Count = rest(map)?;
                                Event::TokenCount {
                                    info: c.info,
                                    rate_limits: c.rate_limits,
                                }
                            }
                            "patch_apply_end" => {
                                let p: Patch = rest(map)?;
                                Event::PatchApplyEnd {
                                    call_id: p.call_id,
                                    status: p.status,
                                    success: p.success,
                                }
                            }
                            "item_completed" => Event::ItemCompleted {
                                item: rest::<_, Completed>(map)?.item.unwrap_or(Item::Untagged),
                            },
                            _ => {
                                skip(map)?;
                                Event::Other(kind)
                            }
                        })
                    },
                    Event::Untagged,
                )
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}

/// `item_completed` 的 `item`（`TurnItem`，标记是变体名）。
pub(super) enum Item<'a> {
    UserMessage {
        text: Text<'a>,
    },
    AgentMessage {
        text: Text<'a>,
    },
    /// `id` 是 `apply_patch` 调用的 `call_id`。`changes` 只在 `load` 里读。
    FileChange {
        id: Option<Cow<'a, str>>,
        status: Option<Cow<'a, str>>,
    },
    Other(Cow<'a, str>),
    Untagged,
}

impl<'de: 'a, 'a> Deserialize<'de> for Item<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Message<'a> {
            #[serde(borrow, default)]
            content: Parts<'a>,
        }
        #[derive(Deserialize)]
        struct Change<'a> {
            #[serde(borrow)]
            id: Option<Cow<'a, str>>,
            #[serde(borrow)]
            status: Option<Cow<'a, str>>,
        }
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Item<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a turn item")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                tagged(
                    map,
                    |kind, map| {
                        Ok(match &*kind {
                            "UserMessage" => Item::UserMessage {
                                text: rest::<_, Message>(map)?.content.0,
                            },
                            "AgentMessage" => Item::AgentMessage {
                                text: rest::<_, Message>(map)?.content.0,
                            },
                            "FileChange" => {
                                let c: Change = rest(map)?;
                                Item::FileChange {
                                    id: c.id,
                                    status: c.status,
                                }
                            }
                            _ => {
                                skip(map)?;
                                Item::Other(kind)
                            }
                        })
                    },
                    Item::Untagged,
                )
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}

/// `UserMessage` / `AgentMessage` 的 `content`：第一个带 `text` 的部分的开头（预览和是否为空）。
#[derive(Default)]
struct Parts<'a>(Text<'a>);

impl<'de: 'a, 'a> Deserialize<'de> for Parts<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Part<'a> {
            #[serde(borrow)]
            text: Option<Text<'a>>,
        }
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Parts<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("message content")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut first: Option<Text<'a>> = None;
                let mut blank = true;
                while let Some(part) = seq.next_element::<Part<'de>>()? {
                    if let Some(text) = part.text {
                        blank &= text.blank;
                        if first.is_none() {
                            first = Some(text);
                        }
                    }
                }
                let mut text = first.unwrap_or_default();
                text.blank = blank;
                Ok(Parts(text))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Parts::default())
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// `response_item` 的 `payload`。
pub(super) enum Response<'a> {
    /// `function_call`：`arguments` 是装着 JSON 的字符串，留原样，只有分类表里的工具才解析。
    FunctionCall {
        name: Cow<'a, str>,
        arguments: Option<&'a RawValue>,
        call_id: Option<Cow<'a, str>>,
    },
    /// `custom_tool_call`：`input` 是原样的字符串（`apply_patch` 的补丁）。
    CustomToolCall {
        name: Cow<'a, str>,
        input: Option<&'a RawValue>,
        call_id: Option<Cow<'a, str>>,
    },
    LocalShellCall {
        call_id: Option<Cow<'a, str>>,
        status: Option<Cow<'a, str>>,
        action: Option<ShellAction<'a>>,
    },
    /// `function_call_output` 或 `custom_tool_call_output`。
    Output {
        call_id: Option<Cow<'a, str>>,
        output: Output<'a>,
    },
    Other(Cow<'a, str>),
    Untagged,
}

impl<'de: 'a, 'a> Deserialize<'de> for Response<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Call<'a> {
            #[serde(borrow)]
            name: Option<Cow<'a, str>>,
            #[serde(borrow)]
            arguments: Option<&'a RawValue>,
            #[serde(borrow)]
            input: Option<&'a RawValue>,
            #[serde(borrow)]
            call_id: Option<Cow<'a, str>>,
        }
        #[derive(Deserialize)]
        struct Shell<'a> {
            #[serde(borrow)]
            call_id: Option<Cow<'a, str>>,
            #[serde(borrow)]
            id: Option<Cow<'a, str>>,
            #[serde(borrow)]
            status: Option<Cow<'a, str>>,
            #[serde(borrow)]
            action: Option<ShellAction<'a>>,
        }
        #[derive(Deserialize)]
        struct Out<'a> {
            #[serde(borrow)]
            call_id: Option<Cow<'a, str>>,
            #[serde(borrow, default)]
            output: Output<'a>,
        }
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Response<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a response item")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                tagged(
                    map,
                    |kind, map| {
                        Ok(match &*kind {
                            "function_call" => {
                                let c: Call = rest(map)?;
                                Response::FunctionCall {
                                    name: c.name.unwrap_or_default(),
                                    arguments: c.arguments,
                                    call_id: c.call_id,
                                }
                            }
                            "custom_tool_call" => {
                                let c: Call = rest(map)?;
                                Response::CustomToolCall {
                                    name: c.name.unwrap_or_default(),
                                    input: c.input,
                                    call_id: c.call_id,
                                }
                            }
                            "local_shell_call" => {
                                let s: Shell = rest(map)?;
                                Response::LocalShellCall {
                                    call_id: s.call_id.or(s.id),
                                    status: s.status,
                                    action: s.action,
                                }
                            }
                            "function_call_output" | "custom_tool_call_output" => {
                                let o: Out = rest(map)?;
                                Response::Output {
                                    call_id: o.call_id,
                                    output: o.output,
                                }
                            }
                            _ => {
                                skip(map)?;
                                Response::Other(kind)
                            }
                        })
                    },
                    Response::Untagged,
                )
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}

/// `local_shell_call.action`（`{"type": "exec", command, working_directory}`）。
#[derive(Deserialize)]
pub(super) struct ShellAction<'a> {
    #[serde(borrow)]
    pub command: Option<Vec<Cow<'a, str>>>,
    #[serde(borrow)]
    pub working_directory: Option<Cow<'a, str>>,
}

/// 工具输出：字符串、内容项数组（取第一个带 `text` 的项），或旧版本的 `{content, success}`。
#[derive(Default)]
pub(super) struct Output<'a> {
    pub text: Text<'a>,
    pub success: Option<bool>,
}

impl<'de: 'a, 'a> Deserialize<'de> for Output<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Old<'a> {
            #[serde(borrow, default)]
            content: Output<'a>,
            success: Option<bool>,
        }
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Output<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a tool output")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(Output {
                    text: Text::new(Cow::Borrowed(v)),
                    success: None,
                })
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Output {
                    text: Text::owned(v),
                    success: None,
                })
            }
            fn visit_seq<S: SeqAccess<'de>>(self, seq: S) -> Result<Self::Value, S::Error> {
                let parts = Parts::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
                Ok(Output {
                    text: parts.0,
                    success: None,
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let old: Old = rest(map)?;
                Ok(Output {
                    text: old.content.text,
                    success: old.success,
                })
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Output::default())
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// 读取路径上只需要文本的开头和末尾：prompt 的预览、是否为空、输出开头的退出码与 shell 会话，
/// 旧版本 JSON 输出末尾的 `metadata.exit_code`。
#[derive(Default)]
pub(super) struct Text<'a> {
    /// 去掉开头空白之后的最多 [`Text::HEAD`] 字节。
    pub head: Cow<'a, str>,
    /// 最后最多 [`Text::TAIL`] 字节（整段比开头短时为空）。
    pub tail: Cow<'a, str>,
    pub blank: bool,
}

impl<'a> Text<'a> {
    const HEAD: usize = 2048;
    const TAIL: usize = 256;

    fn new(s: Cow<'a, str>) -> Self {
        match s {
            Cow::Borrowed(s) => {
                let (head, tail) = split(s);
                Text {
                    head: Cow::Borrowed(head),
                    tail: Cow::Borrowed(tail),
                    blank: s.trim().is_empty(),
                }
            }
            Cow::Owned(s) => Text::owned(&s),
        }
    }

    fn owned(s: &str) -> Self {
        let (head, tail) = split(s);
        Text {
            head: Cow::Owned(head.to_owned()),
            tail: Cow::Owned(tail.to_owned()),
            blank: s.trim().is_empty(),
        }
    }
}

fn split(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    if s.len() <= Text::HEAD {
        return (s, "");
    }
    let mut end = Text::HEAD;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut start = s.len() - Text::TAIL;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    (&s[..end], &s[start.max(end)..])
}

impl<'de: 'a, 'a> Deserialize<'de> for Text<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Text<'a>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(Text::new(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Text::owned(v))
            }
        }
        d.deserialize_str(V(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_format_lines_pick_the_payload_by_type() {
        let line: Line = serde_json::from_str(
            r#"{"timestamp":"t","ordinal":3,"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","model_context_window":1}}"#,
        )
        .unwrap();
        assert_eq!(line.ordinal, Some(3));
        assert!(matches!(
            line.payload,
            Payload::Event(Event::TurnStarted { turn_id: Some(ref t) }) if t == "turn-1"
        ));
        // `payload` 在 `type` 之前。
        let line: Line = serde_json::from_str(
            r#"{"payload":{"type":"agent_message","message":"hi"},"type":"event_msg","timestamp":"t"}"#,
        )
        .unwrap();
        assert!(matches!(
            line.payload,
            Payload::Event(Event::AgentMessage { ref message }) if message.head == "hi"
        ));
        // 不认识的类型与变体：内容跳过。
        let line: Line =
            serde_json::from_str(r#"{"type":"world_state","payload":{"full":true,"state":{}}}"#)
                .unwrap();
        assert!(matches!(line.payload, Payload::Skipped));
        let line: Line = serde_json::from_str(
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"x":1}],"content":null}}"#,
        )
        .unwrap();
        assert!(
            matches!(line.payload, Payload::Response(Response::Other(ref k)) if k == "reasoning")
        );
        // 标记不在第一个键（键被重新排序过）：缓存其余的键再读，嵌套的条目同样。
        let line: Line = serde_json::from_str(
            r#"{"payload":{"turn_id":"x","type":"task_started"},"type":"event_msg"}"#,
        )
        .unwrap();
        assert!(matches!(
            line.payload,
            Payload::Event(Event::TurnStarted { turn_id: Some(ref t) }) if t == "x"
        ));
        let line: Line = serde_json::from_str(
            r#"{"type":"event_msg","payload":{"item":{"content":[{"text":"hi \"q\"","type":"text"}],"type":"UserMessage"},"type":"item_completed"}}"#,
        )
        .unwrap();
        assert!(matches!(
            line.payload,
            Payload::Event(Event::ItemCompleted { item: Item::UserMessage { ref text } }) if text.head == "hi \"q\""
        ));
        // 没有标记。
        let line: Line =
            serde_json::from_str(r#"{"type":"event_msg","payload":{"turn_id":"x"}}"#).unwrap();
        assert!(matches!(line.payload, Payload::Event(Event::Untagged)));
    }

    #[test]
    fn codex_format_outputs_and_sources() {
        let out: Output = serde_json::from_str(r#""Exit code: 1\nOutput:\nboom""#).unwrap();
        assert!(out.text.head.starts_with("Exit code: 1"));
        let out: Output = serde_json::from_str(
            r#"[{"type":"input_image","image_url":"x"},{"type":"input_text","text":"ok"}]"#,
        )
        .unwrap();
        assert_eq!(out.text.head, "ok");
        let out: Output = serde_json::from_str(r#"{"content":"bad","success":false}"#).unwrap();
        assert_eq!((&*out.text.head, out.success), ("bad", Some(false)));

        let long = format!("\"{}END\"", "a".repeat(10_000));
        let out: Output = serde_json::from_str(&long).unwrap();
        assert_eq!(out.text.head.len(), Text::HEAD);
        assert!(out.text.tail.ends_with("END") && out.text.tail.len() == Text::TAIL);

        let meta: SessionMeta = serde_json::from_str(
            r#"{"id":"a","source":{"subagent":{"thread_spawn":{"parent_thread_id":"p","depth":1}}}}"#,
        )
        .unwrap();
        assert!(meta.source.subagent);
        assert_eq!(meta.source.parent_thread_id.as_deref(), Some("p"));
        let meta: SessionMeta =
            serde_json::from_str(r#"{"id":"a","source":{"subagent":"review"}}"#).unwrap();
        assert!(meta.source.subagent && meta.source.parent_thread_id.is_none());
        let meta: SessionMeta = serde_json::from_str(r#"{"id":"a","source":"cli"}"#).unwrap();
        assert!(!meta.source.subagent);
    }
}
