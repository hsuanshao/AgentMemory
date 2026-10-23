//! Secret redaction, shared by the `amem` client (before a turn is queued or
//! sent) and memory-core (before anything is stored or distilled). Errs
//! toward over-redacting: a lost token in a memory is cheap, a leaked one is
//! not.
//!
//! Three kinds of rules:
//! - token and key shapes (`sk-…`, `ghp_…`, JWTs, private keys, …);
//! - `password=` style assignments with an English key name;
//! - a password or secret named in natural language, in the major languages
//!   of Asia plus English and Spanish, followed by its value
//!   (`資料庫密碼是 Hunter2…`, `mật khẩu: …`, `비밀번호는 …`).
//!
//! A fourth, shape-based rule catches what has no name around it: a run of 8
//! or more ASCII characters mixing letters, digits and a password symbol
//! (see [`looks_like_password`]).
//!
//! Deployments add their own patterns (national ids, phone numbers) with
//! [`Redactor::with_extra`].

use std::sync::OnceLock;

use regex::{Captures, Regex};

/// (kind, pattern). A pattern with a `keep` group keeps that prefix (e.g.
/// the `password=` of a key/value pair) and redacts the rest of the match.
/// An assignment's key may go on after the secret word (`SECRET_KEY_BASE=`,
/// `GITHUB_TOKEN_RO=`, `password1=`), unless a part of it says the value is a
/// setting about the secret, not the secret: `max_tokens=`, `token_count:`
/// (see [`NON_SECRET_PARTS`]).
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
        r#"(?i)(?P<keep>\b(?P<key>[A-Z0-9_.-]*(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret)[A-Z0-9_.-]*)["']?\s*[:=]\s*["']?)[^\s"',;]{6,}"#,
    ),
];

/// Words for a password or secret. Latin-script words are matched as whole
/// words; the others as written, since those scripts do not separate words
/// with spaces.
const SECRET_WORDS_LATIN: &[&str] = &[
    // English, with the abbreviations people type in chat
    "password",
    "passwords",
    "passwd",
    "passcode",
    "passphrase",
    "pwd",
    "pw",
    "passw",
    "psw",
    "pswd",
    "pword",
    "p/w",
    "secret",
    "secret key",
    "api key",
    "apikey",
    "access key",
    "private key",
    "client secret",
    // Spanish
    "contraseña",
    "contrasena",
    "clave secreta",
    // Vietnamese
    "mật khẩu",
    "mat khau",
    "mã bí mật",
    "khóa bí mật",
    // Indonesian / Malay
    "kata sandi",
    "kata laluan",
    "sandi",
    // Portuguese, French, German, Turkish
    "senha",
    "mot de passe",
    "passwort",
    "kennwort",
    "şifre",
];

const SECRET_WORDS_OTHER: &[&str] = &[
    // Chinese (traditional, simplified)
    "密碼",
    "密码",
    "口令",
    "密鑰",
    "密钥",
    "金鑰",
    "私鑰",
    "私钥",
    // Japanese
    "パスワード",
    "パスコード",
    "暗証番号",
    "秘密鍵",
    "アクセスキー",
    // Korean
    "비밀번호",
    "비번",
    "패스워드",
    "암호",
    "비밀키",
    "비밀 키",
    // Thai
    "รหัสผ่าน",
    "รหัสลับ",
    // Hindi
    "पासवर्ड",
    "कूटशब्द",
    // Arabic
    "كلمة المرور",
    "كلمة السر",
    "الرقم السري",
    "رمز سري",
    // Russian
    "пароль",
];

/// Words that also mean something ordinary: `token 4096`, `tests pass 100ms`,
/// `รหัส 12345` (a code), `パス` (a path). They count only before a strong
/// separator or an "is" word, never before a bare space.
const AMBIGUOUS_WORDS_LATIN: &[&str] = &["token", "pass", "mk", "clave", "parola"];
const AMBIGUOUS_WORDS_OTHER: &[&str] = &["令牌", "權杖", "パス", "トークン", "토큰", "รหัส"];

/// "is" between the word and the value: Latin-script ones need spaces
/// around them, the others may touch the word (`密碼是`, `비밀번호는`).
const CONNECTORS_LATIN: &[&str] = &[
    "is", "was", "are", "es", "era", "é", "est", "ist", "lautet", "là", "adalah", "ialah",
];
const CONNECTORS_OTHER: &[&str] = &[
    "是",
    "為",
    "为",
    "係",
    "は",
    "が",
    "는",
    "은",
    "이",
    "가",
    "คือ",
    "هي",
    "هو",
    "है",
];

fn alternation(words: &[&str]) -> String {
    let mut words: Vec<&str> = words.to_vec();
    // Longest first, so `clave secreta` wins over `clave`.
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    words
        .iter()
        .map(|w| regex::escape(w).replace(' ', r"\s+"))
        .collect::<Vec<_>>()
        .join("|")
}

/// The natural-language rule: a secret word, then either a strong separator
/// (`:`, `=`, `：`), after which any value is redacted, or a weak one (an
/// "is" word or just a space), after which only a value that looks like a
/// secret is (see [`looks_like_secret`]). An ambiguous word takes no bare
/// space. Values are ASCII: a CJK word after `密碼是` is a sentence going on,
/// not a password.
fn secret_phrase_pattern() -> String {
    let words = |latin: &[&str], other: &[&str]| {
        format!(
            r"(?:\b(?:{})\b|(?:{}))",
            alternation(latin),
            alternation(other)
        )
    };
    let word = words(SECRET_WORDS_LATIN, SECRET_WORDS_OTHER);
    let ambiguous = words(AMBIGUOUS_WORDS_LATIN, AMBIGUOUS_WORDS_OTHER);
    let connector = format!(
        r"\s+(?:{})\s+|\s*(?:{})\s*",
        alternation(CONNECTORS_LATIN),
        alternation(CONNECTORS_OTHER)
    );
    // Group names must be unique, hence `strong` and `strong2`.
    let strong = |name: &str| format!(r#"(?P<{name}>\s*[:=：]\s*["']?)"#);
    let (strong1, strong2) = (strong("strong"), strong("strong2"));
    format!(
        r#"(?i)(?P<keep>{word}(?:{strong1}|{connector}|\s+)|{ambiguous}(?:{strong2}|{connector}))(?P<value>[A-Za-z0-9][!-~&&[^"'`,;)\]}}]]{{3,}})"#
    )
}

/// A value after a weak separator is redacted only when it looks like a
/// secret: a digit, a symbol, or upper and lower case mixed. That keeps
/// "the password policy" and "token budget" readable.
fn looks_like_secret(value: &str) -> bool {
    let digit = value.chars().any(|c| c.is_ascii_digit());
    let symbol = value
        .chars()
        .any(|c| c.is_ascii_punctuation() && !matches!(c, '.' | '!' | '?'));
    let upper = value.chars().any(|c| c.is_ascii_uppercase());
    let lower = value.chars().any(|c| c.is_ascii_lowercase());
    digit || symbol || (upper && lower && value.chars().skip(1).any(|c| c.is_ascii_uppercase()))
}

/// Symbols that make a string look like a password. `-`, `_`, `.`, `/` and
/// `:` are left out: identifiers, versions and paths are full of them.
const PASSWORD_SYMBOLS: &str = "!@#$%^&+=?~|";

/// Trimmed off a run before it is judged: quotes, brackets and markdown
/// around a token are not part of it.
const WRAPPING: &[char] = &[
    '`', '\'', '"', '(', ')', '[', ']', '{', '}', '<', '>', ',', '.', ';', ':', '*', '_',
];

/// Model names, open and commercial: `4@qwen3-8b` is a model, not a password.
/// Only a whole model reference counts (see [`model_reference`]), so
/// `Falcon#2024x` and `gptW0rk!ng#1` are still passwords.
const MODEL_NAMES: &[&str] = &[
    "llama",
    "qwen",
    "mistral",
    "mixtral",
    "codestral",
    "gemma",
    "deepseek",
    "nemotron",
    "phi",
    "glm",
    "chatglm",
    "baichuan",
    "internlm",
    "falcon",
    "olmo",
    "starcoder",
    "codellama",
    "minimax",
    "kimi",
    "granite",
    "command-r",
    "solar",
    "exaone",
    "hunyuan",
    "ernie",
    "openchat",
    "vicuna",
    "wizardlm",
    "hermes",
    "dolphin",
    "smollm",
    "tinyllama",
    "bge",
    "jina",
    "nomic",
    "whisper",
    "sdxl",
    "flux",
    "stable-diffusion",
    "gpt",
    "claude",
    "opus",
    "sonnet",
    "haiku",
    "gemini",
    "grok",
];

/// A whole run that names a model: an optional count (`4@`), a model name
/// followed by nothing, a digit or a separator and then a version
/// (`qwen3-8b`, `llama-3.1-70b`), and optionally `@` and another model, a
/// quantization (`q4`) or a dotted version (`2.5-pro`).
fn model_reference() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let model = format!(r"(?:{})(?:[-_.:0-9][\w.:-]*)?", alternation(MODEL_NAMES));
        Regex::new(&format!(
            r"(?i)^(?:\d+@)?{model}(?:@(?:{model}|q\d\w*|\d+(?:\.\d+)+[\w.-]*))?$"
        ))
        .expect("valid model reference pattern")
    })
}

/// Text that has a password's symbols but is something else: an email
/// address, a query string, or code (`cmd1&&cmd2`, `a1!=b2`, `C++17&&x86`).
/// Code means identifiers or numbers joined by operators; `Tr0ub4dor&&3x`
/// and `P@ss==w0rd9` do not qualify.
fn not_a_password() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"^[\w.+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+$",
            r"|^[?&]?[\w.-]+=[^&=]*(?:&[\w.-]+=[^&=]*)+$",
            r"|^(?:[A-Za-z_][\w.]*(?:\+\+\d*)?|\d+(?:\.\d+)?)(?:(?:&&|\|\||!=|==)(?:[A-Za-z_][\w.]*(?:\+\+\d*)?|\d+(?:\.\d+)?))+$",
        ))
        .expect("valid exclusion pattern")
    })
}

/// A run of ASCII that looks like a password: once trimmed, at least 8
/// characters with a letter, a digit and a password symbol somewhere inside
/// (not first or last, so `#57`, `~15s`, `@someone` and `50%` stay). URLs,
/// paths, `x=1` style settings, model names, email addresses, query strings
/// and code operators are not passwords.
fn looks_like_password(core: &str) -> bool {
    if core.len() < 8 || core.contains("://") || core.contains('/') || core.contains('\\') {
        return false;
    }
    let inner = &core[1..core.len() - 1];
    let symbols: Vec<char> = inner
        .chars()
        .filter(|c| PASSWORD_SYMBOLS.contains(*c))
        .collect();
    if symbols.is_empty() || symbols.iter().all(|c| *c == '=') {
        return false;
    }
    if model_reference().is_match(core) || not_a_password().is_match(core) {
        return false;
    }
    core.chars().any(|c| c.is_ascii_alphabetic()) && core.chars().any(|c| c.is_ascii_digit())
}

#[derive(Clone, Copy, PartialEq)]
enum Check {
    /// The pattern alone decides.
    None,
    /// The natural-language rule: weak-separator matches go through
    /// [`looks_like_secret`].
    Phrase,
    /// `secret-assignment`: a key with a part from [`NON_SECRET_PARTS`] is
    /// kept. Rust's regex has no lookahead, so this is checked here.
    Assignment,
    /// The shape rule: every ASCII run goes through [`looks_like_password`].
    Shape,
}

struct Rule {
    kind: String,
    re: Regex,
    check: Check,
}

/// A compiled set of rules: the built-in ones, plus any a deployment adds.
pub struct Redactor {
    rules: Vec<Rule>,
}

impl Redactor {
    fn builtin_rules() -> Vec<Rule> {
        let mut rules: Vec<Rule> = RULES
            .iter()
            .map(|(kind, pattern)| Rule {
                kind: (*kind).to_owned(),
                re: Regex::new(pattern).expect("valid redaction pattern"),
                check: match *kind {
                    "secret-assignment" => Check::Assignment,
                    _ => Check::None,
                },
            })
            .collect();
        rules.push(Rule {
            kind: "secret-phrase".into(),
            re: Regex::new(&secret_phrase_pattern()).expect("valid secret phrase pattern"),
            check: Check::Phrase,
        });
        rules
    }

    fn shape_rule() -> Rule {
        Rule {
            kind: "password-shape".into(),
            re: Regex::new("[!-~]+").expect("valid ascii run pattern"),
            check: Check::Shape,
        }
    }

    /// The built-in rules only.
    pub fn builtin() -> &'static Redactor {
        static BUILTIN: OnceLock<Redactor> = OnceLock::new();
        BUILTIN.get_or_init(|| Self::new(&[], true).expect("the built-in rules compile"))
    }

    /// The built-in rules with the shape rule, followed by `extra`.
    pub fn with_extra(extra: &[(String, String)]) -> Result<Redactor, String> {
        Self::new(extra, true)
    }

    /// The built-in rules, the shape rule when `password_shape` is set, then
    /// `extra` (kind, pattern) pairs. A pattern may have a `keep` group, like
    /// the built-in ones. Fails on the first pattern that does not compile,
    /// naming it.
    pub fn new(extra: &[(String, String)], password_shape: bool) -> Result<Redactor, String> {
        let mut rules = Self::builtin_rules();
        if password_shape {
            rules.push(Self::shape_rule());
        }
        for (kind, pattern) in extra {
            let re = Regex::new(pattern)
                .map_err(|e| format!("redaction pattern {kind:?} does not compile: {e}"))?;
            rules.push(Rule {
                kind: kind.clone(),
                re,
                check: Check::None,
            });
        }
        Ok(Redactor { rules })
    }

    /// Replaces every secret with `[REDACTED:<kind>]`.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for rule in &self.rules {
            if !rule.re.is_match(&out) {
                continue;
            }
            out = rule
                .re
                .replace_all(&out, |c: &Captures| match rule.check {
                    Check::Phrase
                        if c.name("strong").is_none()
                            && c.name("strong2").is_none()
                            && !looks_like_secret(&c["value"]) =>
                    {
                        c[0].to_owned()
                    }
                    Check::Assignment if names_a_setting(&c["key"]) => c[0].to_owned(),
                    Check::Shape => {
                        let run = &c[0];
                        let core = run.trim_matches(WRAPPING);
                        if !looks_like_password(core) {
                            return run.to_owned();
                        }
                        let start = run.len() - run.trim_start_matches(WRAPPING).len();
                        let end = start + core.len();
                        format!("{}[REDACTED:{}]{}", &run[..start], rule.kind, &run[end..])
                    }
                    _ => {
                        let keep = c.name("keep").map_or("", |m| m.as_str());
                        format!("{keep}[REDACTED:{}]", rule.kind)
                    }
                })
                .into_owned();
        }
        out
    }
}

/// Field names whose value is a secret whatever it looks like. A key counts
/// when these words appear in it as whole parts (see [`is_secret_key`]).
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "pass",
    "passphrase",
    "passcode",
    "pin",
    "otp",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_key",
    "secret_key",
    "private_key",
    "client_secret",
    "credential",
    "credentials",
    "authorization",
    "cookie",
];

/// Parts of a key that make it a setting about a secret rather than the
/// secret itself: `max_tokens`, `token_count`, `password_min_length`,
/// `pass_rate`.
const NON_SECRET_PARTS: &[&str] = &[
    "tokens", "count", "limit", "budget", "usage", "size", "max", "min", "length", "len", "rate",
    "ratio", "total", "ttl", "timeout", "expiry", "expires", "type",
];

/// A key split into lowercase parts: `dbPassword`, `DB_PASSWORD`,
/// `db-password` and `db.password` all give `["db", "password"]`.
fn key_parts(name: &str) -> Vec<String> {
    let mut snake = String::with_capacity(name.len() + 4);
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() && prev_lower {
            snake.push('_');
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        snake.push(match c {
            '-' | '.' | ' ' => '_',
            c => c.to_ascii_lowercase(),
        });
    }
    snake
        .split('_')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

fn names_a_setting(key: &str) -> bool {
    key_parts(key)
        .iter()
        .any(|p| NON_SECRET_PARTS.contains(&p.as_str()))
}

/// Whether a structured field (a JSON key, a tool argument) holds a secret
/// by its name: `password`, `dbPassword`, `access-token`, `GITHUB_TOKEN_RO`,
/// `SECRET_KEY_BASE`. Its value then has no name around it for the text
/// rules to see, so the caller redacts it whole. `max_tokens` and
/// `token_count` are settings, not secrets.
pub fn is_secret_key(name: &str) -> bool {
    let parts = key_parts(name);
    if parts.iter().any(|p| NON_SECRET_PARTS.contains(&p.as_str())) {
        return false;
    }
    SECRET_KEYS.iter().any(|k| {
        let words: Vec<&str> = k.split('_').collect();
        parts
            .windows(words.len())
            .any(|w| w.iter().zip(&words).all(|(p, k)| p == k))
    })
}

/// Replaces every secret with `[REDACTED:<kind>]`, using the built-in rules.
pub fn redact(text: &str) -> String {
    Redactor::builtin().redact(text)
}

#[cfg(test)]
mod tests {
    use super::{is_secret_key, redact, Redactor};

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
        // The key may go on after the secret word.
        for (text, key) in [
            (
                format!("SECRET_KEY_BASE={}", fake("9f8e", '7', 16)),
                "SECRET_KEY_BASE=",
            ),
            (
                format!("GITHUB_TOKEN_RO={}", fake("abc", '1', 9)),
                "GITHUB_TOKEN_RO=",
            ),
            (format!("api_key_v2={}", fake("abc", '1', 9)), "api_key_v2="),
            (format!("password1={}", fake("abc", '1', 8)), "password1="),
        ] {
            assert_eq!(
                redact(&text),
                format!("{key}[REDACTED:secret-assignment]"),
                "{text}"
            );
        }
        assert_eq!(
            redact(r#"{"api_key": "abc123def456"}"#),
            r#"{"api_key": "[REDACTED:secret-assignment]"}"#
        );
        assert_eq!(
            redact(&format!("Authorization: Bearer {}", fake("", 'x', 24))),
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

    /// A password named in natural language, with its value: the cases from
    /// #57 plus one per language.
    #[test]
    fn redacts_secrets_named_in_natural_language() {
        let value = format!("Hunter2{}99", fake("Fake", 'x', 4));
        let cases = [
            format!("資料庫密碼是 {value}"),
            format!("数据库密码是{value}"),
            format!("密碼：{value}"),
            format!("mật khẩu: {value}"),
            format!("mật khẩu là {value}"),
            format!("pw {value}"),
            format!("pwd: {value}"),
            format!("my pass is {value}"),
            format!("psw={value}"),
            format!("p/w {value}"),
            format!("mk: {value}"),
            format!("パス：{value}"),
            format!("비번은 {value}"),
            format!("รหัส: {value}"),
            format!("token: {value}"),
            format!("トークンは {value}"),
            format!("the password is {value}"),
            format!("la contraseña es {value}"),
            format!("パスワードは {value}"),
            format!("パスワード：{value}"),
            format!("비밀번호는 {value}"),
            format!("비밀번호: {value}"),
            format!("รหัสผ่านคือ {value}"),
            format!("kata sandi adalah {value}"),
            format!("पासवर्ड {value} है"),
            format!("كلمة المرور هي {value}"),
            format!("пароль: {value}"),
            format!("a senha é {value}"),
            format!("le mot de passe est {value}"),
            format!("das Passwort lautet {value}"),
        ];
        for input in cases {
            let out = redact(&input);
            assert!(!out.contains(&value), "{input} → {out}");
            assert!(out.contains("[REDACTED:"), "{input} → {out}");
        }
    }

    #[test]
    fn keeps_the_secret_word_and_the_rest_of_the_sentence() {
        assert_eq!(
            redact("資料庫密碼是 Hunter2xyz99，請勿外流"),
            "資料庫密碼是 [REDACTED:secret-phrase]，請勿外流"
        );
        assert_eq!(
            redact("the wifi password is Tr0ub4dor today"),
            "the wifi password is [REDACTED:secret-phrase] today"
        );
    }

    /// Talking about passwords is not a password.
    #[test]
    fn leaves_sentences_about_secrets_alone() {
        for text in [
            "the password policy requires rotation every 90 days",
            "密碼是什麼？請問管理員",
            "記得每季更換密碼",
            "パスワードを忘れました",
            "비밀번호를 잊어버렸어요",
            "the token budget is large",
            "la contraseña es segura",
            "mật khẩu là gì vậy",
            "Monday is the deadline",
            "the API key rotation is scheduled",
            "please pass the review today",
            "did the build pass on Monday",
            "パスタは美味しい",
            "mk dir later",
            "token 4096 is the limit",
            "tests pass 100ms faster",
            "รหัส 12345 สินค้า",
            "max_tokens=128000 and token_count: 52000",
            "MAX_TOKENS=4096 password_min_length=12 token_type: Bearer1",
        ] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn knows_secret_field_names() {
        for key in [
            "password",
            "dbPassword",
            "DB_PASSWORD",
            "access-token",
            "GITHUB_TOKEN",
            "apiKey",
            "client_secret",
            "Authorization",
            "set-cookie",
            "SECRET_KEY_BASE",
            "GITHUB_TOKEN_RO",
            "pass",
            "pin",
            "otp",
        ] {
            assert!(is_secret_key(key), "{key}");
        }
        for key in [
            "max_tokens",
            "token_count",
            "input_tokens",
            "tokenizer",
            "sort_key",
            "secretary",
            "command",
            "pass_rate",
            "password_min_length",
            "spinner",
        ] {
            assert!(!is_secret_key(key), "{key}");
        }
    }

    #[test]
    fn extra_patterns_are_applied_after_the_built_in_ones() {
        let r = Redactor::with_extra(&[
            ("tw-national-id".into(), r"\b[A-Z][12]\d{8}\b".into()),
            (
                "phone".into(),
                r"(?P<keep>\btel:\s*)\+?\d[\d -]{7,}\d".into(),
            ),
        ])
        .unwrap();
        assert_eq!(
            r.redact("ID A123456789, tel: +886 912 345 678"),
            "ID [REDACTED:tw-national-id], tel: [REDACTED:phone]"
        );
        let err = Redactor::with_extra(&[("broken".into(), "(".into())])
            .err()
            .unwrap();
        assert!(err.contains("\"broken\""), "{err}");
    }

    /// No word around it, but the shape of a password.
    #[test]
    fn redacts_password_shaped_strings() {
        let cases = [
            (
                "login with Tr0ub4dor&3x please",
                "login with [REDACTED:password-shape] please",
            ),
            ("設定成ab12!xyz9了", "設定成[REDACTED:password-shape]了"),
            ("(Hk3$9aB2qZ)", "([REDACTED:password-shape])"),
            // A model name inside is not enough to pass.
            ("Falcon#2024x", "[REDACTED:password-shape]"),
            ("Solar@2025ab", "[REDACTED:password-shape]"),
            ("Hermes!2024z", "[REDACTED:password-shape]"),
            ("gptW0rk!ng#1", "[REDACTED:password-shape]"),
            // Operators inside a password do not make it code.
            ("Tr0ub4dor&&3x", "[REDACTED:password-shape]"),
            ("P@ss==w0rd9", "[REDACTED:password-shape]"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact(input), expected, "{input}");
        }
    }

    /// Identifiers, versions, models and references keep their meaning.
    #[test]
    fn leaves_identifiers_and_model_names_alone() {
        for text in [
            "see #57 and #29",
            "it took ~15s and 50% of the budget",
            "@blackhorseya reviewed it",
            "agt-3682683b8133 and scn-20121e3d7c1e4c88",
            "commit 0f58625b8da9cb4e305e7d750efa580c598b3590",
            "v0.4.1-rc1 on pg17-x86_64",
            "run gpt-4o-mini, then qwen3.8-flash-next-iq3_s",
            "4@qwen3-8b vs 2@llama-3.1-70b",
            "Claude@Opus-5.5 and gemini@2.5-pro",
            "nemotron-3-super-120b@q4",
            "https://example.com/a1!b?x=1&y=2",
            "src/a1!b2c3.rs",
            "RUST_LOG=debug2 cargo run",
            "2026-10-23T08:04:03Z",
            "sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e",
            "mail bob.smith1@example.com, foo@bar2.com or user@host1.example",
            "cmd1&&cmd2 then a1!=b2&&c3",
            "GET ?page=2&sort=asc1",
            "C++17&&x86_64",
            "if x1==y2 || z3",
        ] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn the_shape_rule_can_be_turned_off() {
        let r = Redactor::new(&[], false).unwrap();
        assert_eq!(r.redact("Tr0ub4dor&3x"), "Tr0ub4dor&3x");
        assert_eq!(redact("Tr0ub4dor&3x"), "[REDACTED:password-shape]");
    }
}
