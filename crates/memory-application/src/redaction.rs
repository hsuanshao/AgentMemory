//! Which redaction rules a department's writes go through, and the rules
//! its system admins add (#57). Three layers, in this order:
//!
//! 1. the built-in rules (`memory-redact`), always on;
//! 2. the deployment's (`redaction.passwordShape`, `redaction.patterns`);
//! 3. the department's own, managed through `/v3/meta/redaction-rule/*` by
//!    its system admins. Removing one disables it; the record stays.
//!
//! Every write path asks [`RedactionService::redactor`] for the department's
//! redactor. It is cached; a change on this replica takes effect at once,
//! on other replicas within [`CACHE_TTL`]. When the rules cannot be read the
//! write fails rather than going through without them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::SecondsFormat;
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::scope::DepartmentId;
use memory_redact::Redactor;
use serde::{Deserialize, Serialize};

use crate::meta::{require_system_admin, Caller};
use crate::ports::meta::{RedactionRule, RedactionRuleRepository};
use crate::ports::system::{Clock, IdGenerator};

/// How long another replica may go on with rules changed here.
pub const CACHE_TTL: Duration = Duration::from_secs(30);

/// Enabled rules per department, at most.
pub const MAX_RULES: usize = 50;
/// Characters per pattern, at most.
pub const MAX_PATTERN_CHARS: usize = 500;
const MAX_DESCRIPTION_CHARS: usize = 200;
const MAX_TEXT_CHARS: usize = 20_000;

/// The deployment's settings: layer 2.
#[derive(Debug, Clone, Default)]
pub struct DeploymentRules {
    pub password_shape: bool,
    /// (kind, pattern), already checked at startup.
    pub patterns: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternView {
    pub kind: String,
    pub pattern: String,
}

/// The three layers, listed apart.
#[derive(Debug, Clone, Serialize)]
pub struct RedactionRuleListing {
    /// Kinds of the built-in rules; not editable.
    pub builtin: Vec<String>,
    /// The deployment's: the shape rule switch and its patterns; set in
    /// the configuration, not here.
    pub deployment_password_shape: bool,
    pub deployment: Vec<PatternView>,
    /// The department's, enabled or not, oldest first.
    pub department: Vec<RedactionRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRedactionRuleInput {
    pub kind: String,
    pub pattern: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRedactionRuleInput {
    pub rule_id: String,
    pub kind: Option<String>,
    pub pattern: Option<String>,
    pub description: Option<String>,
}

/// Text to run through the department's rules, and optionally a pattern
/// not saved yet, to see what it would catch.
#[derive(Debug, Clone, Deserialize)]
pub struct TestRedactionInput {
    pub text: String,
    pub pattern: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RedactionHit {
    pub kind: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestRedactionResult {
    pub redacted: String,
    pub hits: Vec<RedactionHit>,
}

pub struct RedactionService {
    deployment: DeploymentRules,
    rules: Arc<dyn RedactionRuleRepository>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    cache: Mutex<HashMap<DepartmentId, (Arc<Redactor>, Instant)>>,
}

impl RedactionService {
    pub fn new(
        deployment: DeploymentRules,
        rules: Arc<dyn RedactionRuleRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            deployment,
            rules,
            clock,
            ids,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The department's redactor: all three layers.
    pub async fn redactor(&self, department: &DepartmentId) -> DomainResult<Arc<Redactor>> {
        if let Some(r) = self.cached(department) {
            return Ok(r);
        }
        let rules = self.rules.list_redaction_rules(department).await?;
        let redactor = Arc::new(self.build(&rules, None)?);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(department.clone(), (Arc::clone(&redactor), Instant::now()));
        }
        Ok(redactor)
    }

    fn cached(&self, department: &DepartmentId) -> Option<Arc<Redactor>> {
        let cache = self.cache.lock().ok()?;
        let (redactor, at) = cache.get(department)?;
        (at.elapsed() < CACHE_TTL).then(|| Arc::clone(redactor))
    }

    fn forget(&self, department: &DepartmentId) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(department);
        }
    }

    /// Layers 1–3, plus `extra` (a pattern under test). A stored rule that
    /// no longer compiles (a newer regex crate) fails the build, so writes
    /// stop rather than going through without it.
    fn build(
        &self,
        rules: &[RedactionRule],
        extra: Option<(String, String)>,
    ) -> DomainResult<Redactor> {
        let mut patterns = self.deployment.patterns.clone();
        patterns.extend(
            rules
                .iter()
                .filter(|r| r.enabled)
                .map(|r| (r.kind.clone(), r.pattern.clone())),
        );
        patterns.extend(extra);
        Redactor::new(&patterns, self.deployment.password_shape)
            .map_err(|e| DomainError::internal(format!("redaction rules: {e}")))
    }

    pub async fn list(
        &self,
        department: &DepartmentId,
        caller: &Caller,
    ) -> DomainResult<RedactionRuleListing> {
        require_system_admin(caller)?;
        Ok(RedactionRuleListing {
            builtin: memory_redact::builtin_kinds()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            deployment_password_shape: self.deployment.password_shape,
            deployment: self
                .deployment
                .patterns
                .iter()
                .map(|(kind, pattern)| PatternView {
                    kind: kind.clone(),
                    pattern: pattern.clone(),
                })
                .collect(),
            department: self.rules.list_redaction_rules(department).await?,
        })
    }

    pub async fn create(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: CreateRedactionRuleInput,
    ) -> DomainResult<RedactionRule> {
        require_system_admin(caller)?;
        check_rule(&input.kind, &input.pattern, &input.description)?;
        let rules = self.rules.list_redaction_rules(department).await?;
        check_room(&rules, None)?;
        let now = self.now();
        let rule = RedactionRule {
            rule_id: format!("rdr-{}", self.ids.hex(12)),
            kind: input.kind,
            pattern: input.pattern,
            description: input.description,
            enabled: true,
            created_by: caller.user_id.clone(),
            created_at: now.clone(),
            updated_by: caller.user_id.clone(),
            updated_at: now,
        };
        self.save(department, &rule).await?;
        Ok(rule)
    }

    pub async fn update(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: UpdateRedactionRuleInput,
    ) -> DomainResult<RedactionRule> {
        require_system_admin(caller)?;
        let mut rule = self.find(department, &input.rule_id).await?;
        if let Some(kind) = input.kind {
            rule.kind = kind;
        }
        if let Some(pattern) = input.pattern {
            rule.pattern = pattern;
        }
        if let Some(description) = input.description {
            rule.description = description;
        }
        check_rule(&rule.kind, &rule.pattern, &rule.description)?;
        self.touch(&mut rule, caller);
        self.save(department, &rule).await?;
        Ok(rule)
    }

    /// Disables (`enabled = false`, the "delete") or enables again a rule.
    pub async fn set_enabled(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        rule_id: &str,
        enabled: bool,
    ) -> DomainResult<RedactionRule> {
        require_system_admin(caller)?;
        let mut rule = self.find(department, rule_id).await?;
        if rule.enabled == enabled {
            return Ok(rule);
        }
        if enabled {
            check_room(
                &self.rules.list_redaction_rules(department).await?,
                Some(rule_id),
            )?;
            // Checked again: the regex crate may have changed since.
            check_rule(&rule.kind, &rule.pattern, &rule.description)?;
        }
        rule.enabled = enabled;
        self.touch(&mut rule, caller);
        self.save(department, &rule).await?;
        Ok(rule)
    }

    /// Runs `text` through the department's rules, plus `pattern` when
    /// given, without saving anything. The text is not stored or logged.
    pub async fn test(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: TestRedactionInput,
    ) -> DomainResult<TestRedactionResult> {
        require_system_admin(caller)?;
        if input.text.chars().count() > MAX_TEXT_CHARS {
            return Err(DomainError::invalid(format!(
                "text is limited to {MAX_TEXT_CHARS} characters"
            )));
        }
        let extra = match input.pattern {
            Some(pattern) => {
                let kind = input.kind.unwrap_or_else(|| "preview".into());
                check_rule(&kind, &pattern, "")?;
                Some((kind, pattern))
            }
            None => None,
        };
        let rules = self.rules.list_redaction_rules(department).await?;
        let (redacted, hits) = self.build(&rules, extra)?.redact_counted(&input.text);
        Ok(TestRedactionResult {
            redacted,
            hits: hits
                .into_iter()
                .map(|(kind, count)| RedactionHit { kind, count })
                .collect(),
        })
    }

    async fn find(&self, department: &DepartmentId, rule_id: &str) -> DomainResult<RedactionRule> {
        self.rules
            .list_redaction_rules(department)
            .await?
            .into_iter()
            .find(|r| r.rule_id == rule_id)
            .ok_or_else(|| DomainError::not_found("redaction rule", rule_id))
    }

    async fn save(&self, department: &DepartmentId, rule: &RedactionRule) -> DomainResult<()> {
        self.rules.put_redaction_rule(department, rule).await?;
        self.forget(department);
        Ok(())
    }

    fn touch(&self, rule: &mut RedactionRule, caller: &Caller) {
        rule.updated_by = caller.user_id.clone();
        rule.updated_at = self.now();
    }

    fn now(&self) -> String {
        self.clock
            .now()
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    }
}

/// A kind is what `[REDACTED:<kind>]` shows: short, lowercase, and not one
/// of the built-in ones, so a marker always says which layer caught it.
fn check_rule(kind: &str, pattern: &str, description: &str) -> DomainResult<()> {
    let valid_kind = (1..=40).contains(&kind.len())
        && kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !kind.starts_with('-');
    if !valid_kind {
        return Err(DomainError::invalid(
            "kind must be 1-40 characters of a-z, 0-9 and -",
        ));
    }
    if memory_redact::builtin_kinds().contains(&kind) {
        return Err(DomainError::invalid(format!("{kind} is a built-in kind")));
    }
    if pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err(DomainError::invalid(format!(
            "pattern is limited to {MAX_PATTERN_CHARS} characters"
        )));
    }
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(DomainError::invalid(format!(
            "description is limited to {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    memory_redact::check_pattern(pattern)
        .map_err(|e| DomainError::invalid(format!("pattern is rejected: {e}")))
}

/// Room for one more enabled rule (`except`: the rule being enabled).
fn check_room(rules: &[RedactionRule], except: Option<&str>) -> DomainResult<()> {
    let enabled = rules
        .iter()
        .filter(|r| r.enabled && Some(r.rule_id.as_str()) != except)
        .count();
    if enabled >= MAX_RULES {
        return Err(DomainError::conflict(
            "redaction_rule_limit",
            format!("at most {MAX_RULES} enabled rules per department"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, enabled: bool) -> RedactionRule {
        RedactionRule {
            rule_id: id.into(),
            kind: "emp-id".into(),
            pattern: r"\bEMP-\d{6}\b".into(),
            description: String::new(),
            enabled,
            created_by: "u".into(),
            created_at: "t".into(),
            updated_by: "u".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn kinds_patterns_and_descriptions_are_checked() {
        assert!(check_rule("emp-id", r"\bEMP-\d{6}\b", "badge numbers").is_ok());
        for (kind, pattern) in [
            ("", r"x\d"),
            ("Emp", r"x\d"),
            ("emp id", r"x\d"),
            ("-emp", r"x\d"),
            ("jwt", r"x\d"),
            ("emp", "a*"),
            ("emp", "("),
        ] {
            assert!(check_rule(kind, pattern, "").is_err(), "{kind} {pattern}");
        }
        let long = "x".repeat(MAX_PATTERN_CHARS + 1);
        assert!(check_rule("emp", &long, "").is_err());
        assert!(check_rule("emp", r"x\d", &"d".repeat(201)).is_err());
    }

    #[test]
    fn at_most_fifty_enabled_rules() {
        let mut rules: Vec<_> = (0..MAX_RULES).map(|i| rule(&i.to_string(), true)).collect();
        assert!(check_room(&rules, None).is_err());
        // Enabling one of them again is not one more.
        assert!(check_room(&rules, Some("3")).is_ok());
        rules[0].enabled = false;
        rules.push(rule("old", false));
        assert!(
            check_room(&rules, None).is_ok(),
            "disabled rules do not count"
        );
    }
}
