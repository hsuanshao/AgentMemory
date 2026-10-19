//! Secret redaction applied to every captured message before it is queued
//! or sent. Errs toward over-redacting: a lost token in a memory is cheap,
//! a leaked one is not.

use std::sync::OnceLock;

use regex::{Captures, Regex};

/// (kind, pattern). A pattern with a `keep` group keeps that prefix (e.g.
/// the `password=` of a key/value pair) and redacts the rest of the match.
const RULES: &[(&str, &str)] = &[
    (
        "private-key",
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
    ),
    (
        "url-credentials",
        r"(?P<keep>[a-zA-Z][a-zA-Z0-9+.-]*://[^\s:/@]+:)[^\s@/]+@",
    ),
    (
        "jwt",
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ),
    ("anthropic-key", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
    ("memory-user-key", r"\bsk-mem-[A-Za-z0-9_-]{16,}"),
    ("openai-key", r"\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{20,}"),
    (
        "github-token",
        r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})",
    ),
    ("slack-token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
    ("aws-access-key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    ("google-api-key", r"\bAIza[0-9A-Za-z_-]{35}"),
    (
        "bearer-token",
        r"(?i)(?P<keep>\bbearer\s+)[A-Za-z0-9._~+/=-]{16,}",
    ),
    (
        "secret-assignment",
        r#"(?i)(?P<keep>\b[A-Z0-9_.-]*(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret)[A-Z0-9_.-]*["']?\s*[:=]\s*["']?)[^\s"',;]{6,}"#,
    ),
];

fn compiled() -> &'static [(&'static str, Regex)] {
    static RULES_RE: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    RULES_RE.get_or_init(|| {
        RULES
            .iter()
            .map(|(kind, pattern)| (*kind, Regex::new(pattern).expect("valid redaction pattern")))
            .collect()
    })
}

/// Replaces every secret with `[REDACTED:<kind>]`.
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    for (kind, re) in compiled() {
        if !re.is_match(&out) {
            continue;
        }
        out = re
            .replace_all(&out, |c: &Captures| {
                let keep = c.name("keep").map_or("", |m| m.as_str());
                format!("{keep}[REDACTED:{kind}]")
            })
            .into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::redact;

    /// `prefix` followed by `len` filler characters. Fixtures are built at
    /// runtime so the source holds no token-shaped literal for a secret
    /// scanner (GitGuardian, GitHub push protection) to flag.
    fn fake(prefix: &str, filler: char, len: usize) -> String {
        format!("{prefix}{}", filler.to_string().repeat(len))
    }

    #[test]
    fn redacts_known_token_shapes() {
        let jwt = [fake("ey", 'J', 10), fake("ey", 'J', 10), fake("", 's', 10)].join(".");
        let cases = [
            (format!("key {}", fake("sk-ant-", 'x', 24)), "anthropic-key"),
            (
                format!("OPENAI_KEY {}", fake("sk-proj-", 'x', 24)),
                "openai-key",
            ),
            (fake("sk-mem-", 'x', 32), "memory-user-key"),
            (format!("token {}", fake("ghp_", 'x', 36)), "github-token"),
            (fake("AKIA", 'X', 16), "aws-access-key"),
            (fake("xoxb-", 'x', 12), "slack-token"),
            (fake("AIza", 'x', 35), "google-api-key"),
            (jwt, "jwt"),
        ];
        for (input, kind) in cases {
            let input = input.as_str();
            let out = redact(input);
            assert!(
                out.contains(&format!("[REDACTED:{kind}]")),
                "{input} → {out}"
            );
        }
    }

    #[test]
    fn keeps_the_label_of_assignments_and_headers() {
        assert_eq!(
            redact(&format!("DB_PASSWORD={} next", fake("", 'x', 12))),
            "DB_PASSWORD=[REDACTED:secret-assignment] next"
        );
        assert_eq!(
            redact(r#"{"api_key": "abc123def456"}"#),
            r#"{"api_key": "[REDACTED:secret-assignment]"}"#
        );
        assert_eq!(
            redact("Authorization: Bearer abcdefghijklmnopqrstuvwx"),
            "Authorization: Bearer [REDACTED:bearer-token]"
        );
        let url = format!("postgres://memory:{}@db:5432/x", fake("", 'x', 10));
        assert_eq!(
            redact(&url),
            "postgres://memory:[REDACTED:url-credentials]db:5432/x"
        );
    }

    #[test]
    fn redacts_private_keys_even_when_truncated() {
        let header = format!("-----BEGIN {} PRIVATE KEY-----", "OPENSSH");
        let out = redact(&format!("x\n{header}\n{}\nmore", fake("", 'A', 9)));
        assert_eq!(out, "x\n[REDACTED:private-key]");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for text in [
            "We release v2 next Monday; run integration tests first.",
            "the token budget is 4096",
            "see https://example.com/docs and set password: (ask the admin)",
            "我們下週一發布 v2",
        ] {
            assert_eq!(redact(text), text);
        }
    }
}
