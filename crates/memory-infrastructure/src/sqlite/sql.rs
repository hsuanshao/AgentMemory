//! Small helpers for building filtered queries and mapping timestamps.

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::types::Value;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Accumulates `AND`-ed predicates with positional parameters.
#[derive(Default)]
pub struct Where {
    clauses: Vec<String>,
    pub params: Vec<Value>,
}

impl Where {
    pub fn eq(&mut self, column: &str, value: impl Into<Value>) -> &mut Self {
        self.clauses.push(format!("{column} = ?"));
        self.params.push(value.into());
        self
    }

    pub fn eq_opt(&mut self, column: &str, value: Option<impl Into<Value>>) -> &mut Self {
        if let Some(v) = value {
            self.eq(column, v);
        }
        self
    }

    pub fn raw(&mut self, clause: &str, params: impl IntoIterator<Item = Value>) -> &mut Self {
        self.clauses.push(clause.to_owned());
        self.params.extend(params);
        self
    }

    pub fn in_list(&mut self, column: &str, values: &[String]) -> &mut Self {
        if values.is_empty() {
            self.clauses.push("0".into());
        } else {
            let marks = vec!["?"; values.len()].join(",");
            self.clauses.push(format!("{column} IN ({marks})"));
            self.params.extend(values.iter().cloned().map(Value::from));
        }
        self
    }

    pub fn sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.clauses.join(" AND "))
        }
    }

    pub fn with(&self, extra: impl IntoIterator<Item = Value>) -> Vec<Value> {
        let mut params = self.params.clone();
        params.extend(extra);
        params
    }
}

pub fn to_ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

pub fn from_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

pub fn to_doc<T: Serialize>(value: &T) -> rusqlite::Result<String> {
    serde_json::to_string(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

pub fn from_doc<T: DeserializeOwned>(doc: &str) -> rusqlite::Result<T> {
    serde_json::from_str(doc).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn blob_to_embedding(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

fn like_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Escapes `%`/`_` for a `LIKE ? ESCAPE '\'` prefix match.
pub fn like_prefix(prefix: &str) -> String {
    format!("{}%", like_escape(prefix))
}

/// Escapes `%`/`_` for a `LIKE ? ESCAPE '\'` substring match: a name
/// filter matches the characters typed, not a pattern.
pub fn like_contains(part: &str) -> String {
    format!("%{}%", like_escape(part))
}
