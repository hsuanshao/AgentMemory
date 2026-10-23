//! `/v3/session/add`: store one raw conversation turn as an object, without
//! protocol conversion, under
//! `sessions/<source>/<team>/<user>/<session>[/subagents/<id>]/<started>-<rand>.json`.

use std::sync::Arc;

use memory_redact::Redactor;

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::scope::{DepartmentId, SegmentId};
use serde_json::{Map, Value};

use crate::ports::memory::SessionObjectStore;
use crate::ports::system::IdGenerator;

const UNASSIGNED_TEAM: &str = "_unassigned";
const REQUIRED: [&str; 7] = [
    "source",
    "started_at",
    "ended_at",
    "user_query",
    "model",
    "messages",
    "output",
];
const OPTIONAL: [&str; 1] = ["subagent"];

pub struct SessionTurnHeaders<'a> {
    pub user_id: &'a str,
    pub session_id: &'a str,
    pub team_id: Option<&'a str>,
}

pub struct StoredTurn {
    pub object_key: String,
    pub bytes: usize,
}

pub struct SessionService {
    redactor: Arc<Redactor>,
    store: Arc<dyn SessionObjectStore>,
    ids: Arc<dyn IdGenerator>,
}

impl SessionService {
    pub fn new(
        redactor: Arc<Redactor>,
        store: Arc<dyn SessionObjectStore>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            redactor,
            store,
            ids,
        }
    }

    pub async fn add_turn(
        &self,
        department: &DepartmentId,
        headers: SessionTurnHeaders<'_>,
        body: Value,
    ) -> DomainResult<StoredTurn> {
        let user = SegmentId::parse("x-memory-user-id", headers.user_id)?;
        let session = SegmentId::parse("x-memory-session-id", headers.session_id)?;
        let team = match headers.team_id.filter(|t| !t.is_empty()) {
            Some(t) => SegmentId::parse("x-memory-team-id", t)?,
            None => SegmentId::parse("team", UNASSIGNED_TEAM)?,
        };
        let mut record = validate_record(body)?;
        let source = take_source(&mut record)?;
        let started_at = record["started_at"]
            .as_str()
            .unwrap_or_default()
            .replace(['-', ':'], "");
        let subagent = record
            .get("subagent")
            .map(|s| SegmentId::parse("subagent", s.as_str().unwrap_or_default()))
            .transpose()?;
        for value in record.values_mut() {
            redact_strings(&self.redactor, value);
        }
        record.insert("user".into(), Value::String(user.to_string()));
        record.insert("session".into(), Value::String(session.to_string()));

        let file = format!("{started_at}-{}.json", self.ids.hex(8));
        let base = format!("sessions/{source}/{team}/{user}/{session}");
        let object_key = match subagent {
            Some(sub) => format!("{base}/subagents/{sub}/{file}"),
            None => format!("{base}/{file}"),
        };
        let bytes = serde_json::to_vec(&Value::Object(record))
            .map_err(|e| DomainError::internal(e.to_string()))?;
        self.store.put(department, &object_key, &bytes).await?;
        Ok(StoredTurn {
            object_key,
            bytes: bytes.len(),
        })
    }
}

fn unprocessable(detail: String) -> DomainError {
    DomainError::unprocessable("invalid_session_record", detail)
}

fn validate_record(body: Value) -> DomainResult<Map<String, Value>> {
    let Value::Object(map) = body else {
        return Err(unprocessable("body must be a JSON object".into()));
    };
    for key in map.keys() {
        if key == "user" || key == "session" {
            return Err(unprocessable(format!("field not allowed: {key}")));
        }
        if !REQUIRED.contains(&key.as_str()) && !OPTIONAL.contains(&key.as_str()) {
            return Err(unprocessable(format!("unknown field: {key}")));
        }
    }
    // A missing `source` is a 400 (see `take_source`), the rest are 422.
    for key in REQUIRED.into_iter().filter(|k| *k != "source") {
        if !map.contains_key(key) {
            return Err(unprocessable(format!("missing field: {key}")));
        }
    }
    for key in ["started_at", "ended_at"] {
        if !is_fixed_timestamp(map[key].as_str().unwrap_or_default()) {
            return Err(unprocessable(format!(
                "{key} must be YYYY-MM-DDTHH:MM:SS.sssZ"
            )));
        }
    }
    if !map["user_query"].is_string() {
        return Err(unprocessable("user_query must be a string".into()));
    }
    if map["model"].as_str().is_none_or(str::is_empty) {
        return Err(unprocessable("model must be a non-empty string".into()));
    }
    if map["messages"].as_array().is_none_or(Vec::is_empty) {
        return Err(unprocessable("messages must be a non-empty array".into()));
    }
    if !map["output"].is_object() {
        return Err(unprocessable("output must be an object".into()));
    }
    if map
        .get("subagent")
        .is_some_and(|s| s.as_str().is_none_or(str::is_empty))
    {
        return Err(unprocessable("subagent must be a non-empty string".into()));
    }
    Ok(map)
}

fn take_source(record: &mut Map<String, Value>) -> DomainResult<String> {
    match record.remove("source").as_ref().and_then(Value::as_str) {
        Some(s @ ("online" | "offline")) => Ok(s.to_owned()),
        _ => Err(DomainError::invalid("source must be online or offline")),
    }
}

/// `YYYY-MM-DDTHH:MM:SS.sssZ` exactly.
fn is_fixed_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22];
    b.len() == 24
        && digits.iter().all(|&i| b[i].is_ascii_digit())
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'.'
        && b[23] == b'Z'
}

/// Redacts every string in a raw turn, however deeply nested.
fn redact_strings(redactor: &Redactor, value: &mut Value) {
    match value {
        Value::String(s) => *s = redactor.redact(s),
        Value::Array(items) => items.iter_mut().for_each(|v| redact_strings(redactor, v)),
        Value::Object(map) => map.values_mut().for_each(|v| redact_strings(redactor, v)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> Value {
        json!({
            "source": "online",
            "started_at": "2026-06-11T02:18:20.359Z",
            "ended_at": "2026-06-11T02:18:47.945Z",
            "user_query": "q",
            "model": "m",
            "messages": [{"role": "user", "content": "q"}],
            "output": {"role": "assistant", "content": "a"}
        })
    }

    #[test]
    fn accepts_a_valid_record() {
        assert!(validate_record(body()).is_ok());
        assert!(is_fixed_timestamp("2026-06-11T02:18:20.359Z"));
    }

    #[test]
    fn rejects_identity_fields_and_unknown_fields() {
        let mut b = body();
        b["user"] = json!("x");
        assert!(validate_record(b)
            .unwrap_err()
            .detail
            .contains("not allowed"));
        let mut b = body();
        b["extra"] = json!(1);
        assert!(validate_record(b)
            .unwrap_err()
            .detail
            .contains("unknown field"));
    }

    #[test]
    fn every_string_of_a_raw_turn_is_redacted() {
        let mut turn = body();
        turn["messages"][0]["content"] = json!("資料庫密碼是 Hunter2xyz99");
        turn["output"]["content"] = json!({"nested": ["pw: Tr0ub4dor&3x", 7]});
        redact_strings(Redactor::builtin(), &mut turn);
        let text = turn.to_string();
        assert!(
            !text.contains("Hunter2xyz99") && !text.contains("Tr0ub4dor"),
            "{text}"
        );
        assert_eq!(turn["output"]["content"]["nested"][1], 7);
        assert_eq!(turn["user_query"], "q");
    }
}
