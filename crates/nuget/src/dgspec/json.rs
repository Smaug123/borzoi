//! A JSON tree that keeps what the dgspec reader must see and `serde_json::Value`
//! discards: every member of an object, duplicates included, in document order.
//!
//! NuGet's reader treats a repeated key differently depending on where it sits:
//! a repeated scalar keeps its last value, a repeated dependency name keeps
//! *both* dependencies. `serde_json::Value` would silently keep one, so the
//! typed layer could not even see the repetition it must decline on.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// One JSON value. Numbers keep `serde_json`'s representation, since the only
/// number the dgspec carries is `format`, compared for exact integer equality.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    /// Members in document order, duplicates kept.
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn kind(&self) -> JsonKind {
        match self {
            Json::Null => JsonKind::Null,
            Json::Bool(_) => JsonKind::Bool,
            Json::Number(_) => JsonKind::Number,
            Json::String(_) => JsonKind::String,
            Json::Array(_) => JsonKind::Array,
            Json::Object(_) => JsonKind::Object,
        }
    }
}

/// The type of a JSON value, for declines that name what was expected or found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

impl fmt::Display for JsonKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            JsonKind::Null => "null",
            JsonKind::Bool => "a boolean",
            JsonKind::Number => "a number",
            JsonKind::String => "a string",
            JsonKind::Array => "an array",
            JsonKind::Object => "an object",
        })
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

    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Json, E> {
        serde_json::Number::from_f64(v)
            .map(Json::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Json, E> {
        Ok(Json::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Json, E> {
        Ok(Json::String(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut members = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            members.push((key, value));
        }
        Ok(Json::Object(members))
    }
}

/// Parse `text` as one JSON document.
pub(crate) fn parse(text: &str) -> Result<Json, serde_json::Error> {
    serde_json::from_str(text)
}
