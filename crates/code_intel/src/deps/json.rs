//! A JSON value that keeps the order of an object's keys, for package
//! manifests (「依赖层」): the direct dependencies go in the manifest's
//! order, and `exports` conditions are searched in object order. serde_json's
//! `Value` sorts keys without its `preserve_order` feature, which would add
//! a dependency.

use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// One JSON value. Numbers are not kept: nothing in a manifest needs them.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number,
    String(String),
    Array(Vec<Json>),
    /// Keys in the document's order; a repeated key keeps every occurrence
    /// (`get` takes the first).
    Object(Vec<(String, Json)>),
}

impl Json {
    /// A manifest's bytes as a value; None when they are not JSON.
    pub(crate) fn parse(bytes: &[u8]) -> Option<Json> {
        serde_json::from_slice(bytes).ok()
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn entries(&self) -> &[(String, Json)] {
        match self {
            Json::Object(entries) => entries,
            _ => &[],
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E>(self, _: i64) -> Result<Json, E> {
        Ok(Json::Number)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Json, E> {
        Ok(Json::Number)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Json, E> {
        Ok(Json::Number)
    }

    fn visit_str<E>(self, v: &str) -> Result<Json, E> {
        Ok(Json::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Json, E> {
        Ok(Json::String(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element()? {
            out.push(v);
        }
        Ok(Json::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut out = Vec::new();
        while let Some((k, v)) = map.next_entry::<String, Json>()? {
            out.push((k, v));
        }
        Ok(Json::Object(out))
    }
}
