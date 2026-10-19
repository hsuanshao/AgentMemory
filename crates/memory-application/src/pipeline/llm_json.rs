//! Tolerant extraction of JSON from LLM output (think blocks, code fences,
//! prose around the payload).

use memory_domain::error::{DomainError, DomainResult};
use serde::de::DeserializeOwned;

/// Strips `<think>…</think>` and parses the outermost JSON value that
/// starts with `open` (`[` or `{`).
pub fn parse_llm_json<T: DeserializeOwned>(raw: &str, open: char) -> DomainResult<T> {
    let text = strip_think(raw);
    let close = if open == '[' { ']' } else { '}' };
    let start = text.find(open);
    let end = text.rfind(close);
    let (Some(start), Some(end)) = (start, end) else {
        return Err(DomainError::unprocessable(
            "llm_output_invalid",
            "no JSON payload in LLM output",
        ));
    };
    if end < start {
        return Err(DomainError::unprocessable(
            "llm_output_invalid",
            "malformed JSON payload",
        ));
    }
    serde_json::from_str(&text[start..=end]).or_else(|e| {
        // A payload missing its final closer (seen with Codex: the trailing
        // `]` dropped) makes `rfind` land on an inner bracket and cut an
        // object in half. Retry once with the unclosed brackets balanced.
        balance_closers(&text[start..])
            .and_then(|fixed| serde_json::from_str(&fixed).ok())
            .ok_or_else(|| {
                DomainError::unprocessable(
                    "llm_output_invalid",
                    format!("invalid JSON from LLM: {e}"),
                )
            })
    })
}

/// `raw` (from the opening bracket on, minus a trailing code fence) with
/// the closers of every still-open `[`/`{` appended. `None` when the text
/// ends inside a string or has a stray closer: those are not truncations
/// this can repair.
fn balance_closers(raw: &str) -> Option<String> {
    let body = raw.trim_end();
    let body = body.strip_suffix("```").unwrap_or(body).trim_end();
    let (mut stack, mut in_str, mut escaped) = (Vec::new(), false, false);
    for c in body.chars() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' => stack.push(']'),
            '{' => stack.push('}'),
            ']' | '}' if stack.last() != Some(&c) => return None,
            ']' | '}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    if in_str || stack.is_empty() {
        return None;
    }
    let mut fixed = body.trim_end_matches(',').to_owned();
    fixed.extend(stack.into_iter().rev());
    Some(fixed)
}

pub fn strip_think(raw: &str) -> String {
    let mut text = raw.to_owned();
    while let Some(start) = text.find("<think>") {
        let end = text[start..]
            .find("</think>")
            .map(|e| start + e + "</think>".len())
            .unwrap_or(text.len());
        text.replace_range(start..end, "");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json_after_thinking() {
        let raw = "<think>hmm [x]</think>Sure:\n```json\n[{\"a\":1}]\n```";
        let v: Vec<serde_json::Value> = parse_llm_json(raw, '[').unwrap();
        assert_eq!(v[0]["a"], 1);
    }

    #[test]
    fn repairs_a_dropped_final_closer() {
        let raw = "[{\"t\":\"a\",\"tags\":[\"x\"]},{\"t\":\"b]\",\"tags\":[\"y\",\"z\"]}\n";
        let v: Vec<serde_json::Value> = parse_llm_json(raw, '[').unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1]["tags"][1], "z");
        let fenced = "```json\n[{\"a\":[1]},{\"a\":[2]},\n```";
        let v: Vec<serde_json::Value> = parse_llm_json(fenced, '[').unwrap();
        assert_eq!(v[1]["a"][0], 2);
    }

    #[test]
    fn does_not_repair_a_cut_string() {
        assert!(
            parse_llm_json::<Vec<serde_json::Value>>("[{\"a\":[1]},{\"b\":\"cut", '[').is_err()
        );
    }

    #[test]
    fn errors_without_payload() {
        assert!(parse_llm_json::<Vec<i32>>("nothing", '[').is_err());
    }
}
