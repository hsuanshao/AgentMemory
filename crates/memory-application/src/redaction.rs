//! Which redaction rules a write goes through, and the rules people add
//! (#57). Layers, in this order:
//!
//! 1. the built-in rules (`memory-redact`), always on;
//! 2. the deployment's (`redaction.passwordShape`, `redaction.patterns`);
//! 3. the department's own, for every team, managed by its system admins;
//! 4. a team's, managed by the team's admins (and system admins);
//! 5. a user's, managed by that user (and system admins), applied wherever
//!    the user writes.
//!
//! A layer only adds: none can turn off the ones before it. Scenes and
//! personas are shared by a team's members, so they go through layers 1–4;
//! their input, the members' atoms, already went through each member's.
//! Everyone in the department can read layers 2–3, a team's members its
//! layer 4, and a user their own layer 5. Rules are managed through
//! `/v3/meta/redaction-rule/*`; removing one disables it, the record stays.
//!
//! Every write path asks [`RedactionService::redactor`] for its redactor.
//! It is cached; a change on this replica takes effect at once, on other
//! replicas within [`CACHE_TTL`]. When the rules cannot be read the write
//! fails rather than going through without them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::SecondsFormat;
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::meta::TeamMember;
use memory_domain::scope::DepartmentId;
use memory_redact::Redactor;
use serde::{Deserialize, Serialize};

use crate::meta::{require_system_admin, Caller};
use crate::ports::meta::{MetaStore, RedactionRule, RedactionRuleRepository};
use crate::ports::system::{Clock, IdGenerator};

/// How long another replica may go on with rules changed here.
pub const CACHE_TTL: Duration = Duration::from_secs(30);

/// Enabled rules per level (the department's, one team's, one user's), at most.
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

/// Who a write belongs to, which decides the rules it goes through. Scenes
/// and personas have a team but no user.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteScope<'a> {
    pub team_id: Option<&'a str>,
    pub user_id: Option<&'a str>,
}

impl<'a> WriteScope<'a> {
    /// A team's shared layers: the department's and the team's rules.
    pub fn team(team_id: &'a str) -> Self {
        Self {
            team_id: Some(team_id),
            user_id: None,
        }
    }

    /// A user's writes in a team: the department's, the team's and theirs.
    pub fn member(team_id: &'a str, user_id: &'a str) -> Self {
        Self {
            team_id: Some(team_id),
            user_id: Some(user_id),
        }
    }
}

/// The level rules are listed, added or tried at: neither id for every
/// team, `team_id` for one team, `user_id` for one user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RuleLevel {
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

impl RuleLevel {
    fn of(rule: &RedactionRule) -> Self {
        Self {
            team_id: rule.team_id.clone(),
            user_id: rule.user_id.clone(),
        }
    }

    /// Empty ids are no id; both at once is refused.
    fn normalized(self) -> DomainResult<Self> {
        let level = Self {
            team_id: self.team_id.filter(|t| !t.is_empty()),
            user_id: self.user_id.filter(|u| !u.is_empty()),
        };
        if level.team_id.is_some() && level.user_id.is_some() {
            return Err(DomainError::invalid(
                "a rule is for a team or for a user, not both",
            ));
        }
        Ok(level)
    }

    /// The scope whose rules a test runs: the level's and every level above
    /// it; with both ids, a user's write in that team.
    fn scope(&self) -> WriteScope<'_> {
        WriteScope {
            team_id: self.team_id.as_deref(),
            user_id: self.user_id.as_deref(),
        }
    }
}

/// Whether `rule` is one a write in `scope` goes through (enabled or not).
fn applies(rule: &RedactionRule, scope: WriteScope<'_>) -> bool {
    match (rule.team_id.as_deref(), rule.user_id.as_deref()) {
        (None, None) => true,
        (Some(team), None) => scope.team_id == Some(team),
        (None, Some(user)) => scope.user_id == Some(user),
        (Some(_), Some(_)) => false,
    }
}

/// What rule management needs to know about teams and users.
#[async_trait]
pub trait RedactionDirectory: Send + Sync {
    async fn team_exists(&self, department: &DepartmentId, team_id: &str) -> DomainResult<bool>;
    async fn user_exists(&self, department: &DepartmentId, user_id: &str) -> DomainResult<bool>;
    /// The user's membership of the team, if active.
    async fn active_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<Option<TeamMember>>;
}

#[async_trait]
impl RedactionDirectory for Arc<dyn MetaStore> {
    async fn team_exists(&self, department: &DepartmentId, team_id: &str) -> DomainResult<bool> {
        Ok(self.get_team(department, team_id).await?.is_some())
    }

    async fn user_exists(&self, department: &DepartmentId, user_id: &str) -> DomainResult<bool> {
        Ok(self.get_user(department, user_id).await?.is_some())
    }

    async fn active_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<Option<TeamMember>> {
        Ok(self
            .get_member(department, team_id, user_id)
            .await?
            .filter(TeamMember::is_active))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    View,
    Manage,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternView {
    pub kind: String,
    pub pattern: String,
}

/// The rules a write at the level asked for goes through, layer by layer.
#[derive(Debug, Clone, Serialize)]
pub struct RedactionRuleListing {
    /// Kinds of the built-in rules; not editable.
    pub builtin: Vec<String>,
    /// The deployment's: the shape rule switch and its patterns; set in
    /// the configuration, not here.
    pub deployment_password_shape: bool,
    pub deployment: Vec<PatternView>,
    /// The department's, for every team, enabled or not, oldest first.
    pub department: Vec<RedactionRule>,
    /// Asked with a `team_id`: that team's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<Vec<RedactionRule>>,
    /// Asked with a `user_id`: that user's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<Vec<RedactionRule>>,
    /// Whether the caller may add and change rules at the level asked for.
    pub can_edit: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRedactionRuleInput {
    pub kind: String,
    pub pattern: String,
    #[serde(default)]
    pub description: String,
    #[serde(flatten)]
    pub level: RuleLevel,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRedactionRuleInput {
    pub rule_id: String,
    pub kind: Option<String>,
    pub pattern: Option<String>,
    pub description: Option<String>,
}

/// Text to run through the rules of a level (and the levels above it), and
/// optionally a pattern not saved yet, to see what it would catch. With
/// both ids, a user's write in that team: every level that applies to it.
#[derive(Debug, Clone, Deserialize)]
pub struct TestRedactionInput {
    pub text: String,
    pub pattern: Option<String>,
    pub kind: Option<String>,
    #[serde(flatten)]
    pub level: RuleLevel,
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
    /// With a pattern: the share of the sample it matches, from 0 to 1.
    pub pattern_share: Option<f64>,
    /// With a pattern: why it could not be saved (too broad), if so.
    pub refused: Option<String>,
}

/// A cached redactor: department, team, user ("" for none).
type CacheKey = (DepartmentId, String, String);

pub struct RedactionService {
    deployment: DeploymentRules,
    rules: Arc<dyn RedactionRuleRepository>,
    directory: Arc<dyn RedactionDirectory>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    cache: Mutex<HashMap<CacheKey, (Arc<Redactor>, Instant)>>,
    /// Per department, bumped on every change there. A redactor built from
    /// rules read before a change is not cached after it: a write running
    /// alongside the change would otherwise put the old rules back for the
    /// whole TTL. Its own lock, taken inside `cache`'s lock when both are
    /// needed (never the other way round).
    generations: Mutex<HashMap<DepartmentId, u64>>,
}

impl RedactionService {
    pub fn new(
        deployment: DeploymentRules,
        rules: Arc<dyn RedactionRuleRepository>,
        directory: Arc<dyn RedactionDirectory>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            deployment,
            rules,
            directory,
            clock,
            ids,
            cache: Mutex::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
        }
    }

    /// The redactor of a write in `scope`: every layer that applies to it.
    pub async fn redactor(
        &self,
        department: &DepartmentId,
        scope: WriteScope<'_>,
    ) -> DomainResult<Arc<Redactor>> {
        let key: CacheKey = (
            department.clone(),
            scope.team_id.unwrap_or_default().to_owned(),
            scope.user_id.unwrap_or_default().to_owned(),
        );
        if let Some(r) = self.cached(&key) {
            return Ok(r);
        }
        let generation = self.generation(department);
        let rules = self.rules.list_redaction_rules(department).await?;
        let redactor = Arc::new(self.build(&rules, scope, None)?);
        if let Ok(mut cache) = self.cache.lock() {
            // Checked under the lock `forget` takes, so a change cannot slip
            // in between the check and the insert.
            if self.generation(department) == generation {
                // Expired entries go here too, not only on a change: one per
                // (team, user) that wrote would otherwise stay.
                cache.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
                cache.insert(key, (Arc::clone(&redactor), Instant::now()));
            }
        }
        Ok(redactor)
    }

    fn cached(&self, key: &CacheKey) -> Option<Arc<Redactor>> {
        let cache = self.cache.lock().ok()?;
        let (redactor, at) = cache.get(key)?;
        (at.elapsed() < CACHE_TTL).then(|| Arc::clone(redactor))
    }

    fn generation(&self, department: &DepartmentId) -> u64 {
        self.generations
            .lock()
            .map(|g| g.get(department).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    fn forget(&self, department: &DepartmentId) {
        if let Ok(mut cache) = self.cache.lock() {
            if let Ok(mut generations) = self.generations.lock() {
                *generations.entry(department.clone()).or_insert(0) += 1;
            }
            cache.retain(|(d, _, _), _| d != department);
        }
    }

    /// Layers 1–2, the enabled stored rules that apply to `scope`, and
    /// `extra` (a pattern under test). A stored rule that no longer compiles
    /// (a newer regex crate) fails the build, so writes stop rather than
    /// going through without it.
    fn build(
        &self,
        rules: &[RedactionRule],
        scope: WriteScope<'_>,
        extra: Option<(String, String)>,
    ) -> DomainResult<Redactor> {
        let (active, others): (Vec<&RedactionRule>, Vec<&RedactionRule>) =
            rules.iter().partition(|r| r.enabled && applies(r, scope));
        let mut patterns = self.deployment.patterns.clone();
        patterns.extend(active.iter().map(|r| (r.kind.clone(), r.pattern.clone())));
        patterns.extend(extra);
        // Markers of every other stored kind (disabled since, or another
        // team's or user's, as in scenes built from members' atoms) still
        // count as markers.
        let known: Vec<String> = others.iter().map(|r| r.kind.clone()).collect();
        Redactor::new(&patterns, self.deployment.password_shape)
            .and_then(|r| r.knowing(&known))
            .map_err(|e| DomainError::internal(format!("redaction rules: {e}")))
    }

    /// Checks the caller may read (`View`) or change (`Manage`) the rules
    /// of `level`: the department's are read by everyone and changed by
    /// system admins; a team's by its members and its admins; a user's by
    /// that user. System admins may do all of it.
    async fn authorize(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        level: &RuleLevel,
        access: Access,
    ) -> DomainResult<()> {
        match (level.team_id.as_deref(), level.user_id.as_deref()) {
            (None, None) => match access {
                Access::View => Ok(()),
                Access::Manage => require_system_admin(caller),
            },
            (Some(team), None) => {
                if caller.is_system_admin {
                    return match self.directory.team_exists(department, team).await? {
                        true => Ok(()),
                        false => Err(DomainError::not_found("team", team)),
                    };
                }
                // Membership first: a team that does not exist answers like
                // one the caller is not in, so team ids cannot be probed.
                match self
                    .directory
                    .active_member(department, team, &caller.user_id)
                    .await?
                {
                    None => Err(DomainError::permission_denied(
                        "not an active member of the team",
                    )),
                    Some(m) if access == Access::Manage && !m.is_admin() => {
                        Err(DomainError::permission_denied("team admin required"))
                    }
                    Some(_) => Ok(()),
                }
            }
            (None, Some(user)) => {
                if caller.user_id != user && !caller.is_system_admin {
                    return Err(DomainError::permission_denied(
                        "only the user and system admins",
                    ));
                }
                if !self.directory.user_exists(department, user).await? {
                    return Err(DomainError::not_found("user", user));
                }
                Ok(())
            }
            (Some(_), Some(_)) => Err(DomainError::invalid(
                "a rule is for a team or for a user, not both",
            )),
        }
    }

    /// The rules of `level` and of every level above it.
    pub async fn list(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        level: RuleLevel,
    ) -> DomainResult<RedactionRuleListing> {
        let level = level.normalized()?;
        self.authorize(department, caller, &level, Access::View)
            .await?;
        let can_edit = self
            .authorize(department, caller, &level, Access::Manage)
            .await
            .is_ok();
        let rules = self.rules.list_redaction_rules(department).await?;
        let at = |wanted: &RuleLevel| -> Vec<RedactionRule> {
            rules
                .iter()
                .filter(|r| RuleLevel::of(r) == *wanted)
                .cloned()
                .collect()
        };
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
            department: at(&RuleLevel::default()),
            team: level.team_id.is_some().then(|| at(&level)),
            user: level.user_id.is_some().then(|| at(&level)),
            can_edit,
        })
    }

    pub async fn create(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: CreateRedactionRuleInput,
    ) -> DomainResult<RedactionRule> {
        let level = input.level.normalized()?;
        self.authorize(department, caller, &level, Access::Manage)
            .await?;
        self.check(&input.kind, &input.pattern, &input.description)?;
        let rules = self.rules.list_redaction_rules(department).await?;
        check_kind_level(&rules, &input.kind, &level)?;
        check_room(&rules, &level, None)?;
        let now = self.now();
        let rule = RedactionRule {
            rule_id: format!("rdr-{}", self.ids.hex(12)),
            kind: input.kind,
            pattern: input.pattern,
            description: input.description,
            team_id: level.team_id,
            user_id: level.user_id,
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
        let rules = self.rules.list_redaction_rules(department).await?;
        let mut rule = find(&rules, &input.rule_id)?;
        let level = RuleLevel::of(&rule);
        self.authorize(department, caller, &level, Access::Manage)
            .await?;
        let old_kind = rule.kind.clone();
        if let Some(kind) = input.kind {
            rule.kind = kind;
        }
        if let Some(pattern) = input.pattern {
            rule.pattern = pattern;
        }
        if let Some(description) = input.description {
            rule.description = description;
        }
        self.check(&rule.kind, &rule.pattern, &rule.description)?;
        // Only a new kind is checked: one the rules for all teams took later
        // must not lock the rule's owner out of changing anything else.
        if rule.kind != old_kind {
            check_kind_level(&rules, &rule.kind, &level)?;
        }
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
        let rules = self.rules.list_redaction_rules(department).await?;
        let mut rule = find(&rules, rule_id)?;
        let level = RuleLevel::of(&rule);
        self.authorize(department, caller, &level, Access::Manage)
            .await?;
        if rule.enabled == enabled {
            return Ok(rule);
        }
        if enabled {
            check_room(&rules, &level, Some(rule_id))?;
            // Checked again: the rules may have changed since.
            self.check(&rule.kind, &rule.pattern, &rule.description)?;
        }
        rule.enabled = enabled;
        self.touch(&mut rule, caller);
        self.save(department, &rule).await?;
        Ok(rule)
    }

    /// Runs `text` through the rules of a level and the levels above it,
    /// plus `pattern` when given, without saving anything. The text is not
    /// stored or logged.
    pub async fn test(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: TestRedactionInput,
    ) -> DomainResult<TestRedactionResult> {
        let scope = self.test_scope(department, caller, input.level).await?;
        if input.text.chars().count() > MAX_TEXT_CHARS {
            return Err(DomainError::invalid(format!(
                "text is limited to {MAX_TEXT_CHARS} characters"
            )));
        }
        // A broad pattern is still previewed, so its effect is visible;
        // `refused` says it would not be saved.
        let (mut pattern_share, mut refused) = (None, None);
        let extra = match input.pattern {
            Some(pattern) => {
                let kind = input.kind.unwrap_or_else(|| "preview".into());
                check_rule(&kind, &pattern, "")?;
                pattern_share = Some(
                    memory_redact::coverage(&pattern, &input.text).map_err(DomainError::invalid)?,
                );
                refused = self.check(&kind, &pattern, "").err().map(|e| e.detail);
                Some((kind, pattern))
            }
            None => None,
        };
        let rules = self.rules.list_redaction_rules(department).await?;
        let (redacted, hits) = self
            .build(&rules, scope.scope(), extra)?
            .redact_counted(&input.text);
        Ok(TestRedactionResult {
            redacted,
            hits: hits
                .into_iter()
                .map(|(kind, count)| RedactionHit { kind, count })
                .collect(),
            pattern_share,
            refused,
        })
    }

    /// The rules a test goes through, checked like reading them: one level,
    /// or a user's and a team's together (both must be readable).
    async fn test_scope(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        level: RuleLevel,
    ) -> DomainResult<RuleLevel> {
        let team_id = level.team_id.filter(|t| !t.is_empty());
        let user_id = level.user_id.filter(|u| !u.is_empty());
        if let (Some(team), Some(user)) = (&team_id, &user_id) {
            let user_level = RuleLevel {
                team_id: None,
                user_id: Some(user.clone()),
            };
            let team_level = RuleLevel {
                team_id: Some(team.clone()),
                user_id: None,
            };
            self.authorize(department, caller, &user_level, Access::View)
                .await?;
            self.authorize(department, caller, &team_level, Access::View)
                .await?;
            return Ok(RuleLevel { team_id, user_id });
        }
        let level = RuleLevel { team_id, user_id };
        self.authorize(department, caller, &level, Access::View)
            .await?;
        Ok(level)
    }

    /// What a saved rule must pass: [`check_rule`], a kind that is not the
    /// deployment's either, and a pattern that is not too broad.
    fn check(&self, kind: &str, pattern: &str, description: &str) -> DomainResult<()> {
        check_rule(kind, pattern, description)?;
        if self.deployment.patterns.iter().any(|(k, _)| k == kind) {
            return Err(DomainError::invalid(format!(
                "{kind} is a kind of the deployment's redaction.patterns"
            )));
        }
        memory_redact::check_pattern_breadth(pattern)
            .map_err(|e| DomainError::invalid(format!("pattern is too broad: {e}")))
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

fn find(rules: &[RedactionRule], rule_id: &str) -> DomainResult<RedactionRule> {
    rules
        .iter()
        .find(|r| r.rule_id == rule_id)
        .cloned()
        .ok_or_else(|| DomainError::not_found("redaction rule", rule_id))
}

/// A kind is what `[REDACTED:<kind>]` shows: short, lowercase, and not one
/// of the built-in ones (nor the deployment's, see
/// [`RedactionService::check`]), so a marker always says which layer caught
/// it.
fn check_rule(kind: &str, pattern: &str, description: &str) -> DomainResult<()> {
    memory_redact::check_kind(kind).map_err(DomainError::invalid)?;
    if memory_redact::builtin_kinds().contains(&kind) || kind == memory_redact::SECRET_FIELD_KIND {
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

/// A team's or a user's rule may not take a kind of the rules for all
/// teams, so those markers stay unambiguous. Nothing else is compared:
/// those rules are visible to everyone, while checking against other teams'
/// or users' rules would let anyone take a name first and probe for rules
/// they cannot read. Disabled rules count: their markers stay in stored
/// text.
fn check_kind_level(rules: &[RedactionRule], kind: &str, level: &RuleLevel) -> DomainResult<()> {
    if *level == RuleLevel::default() {
        return Ok(());
    }
    let taken = rules
        .iter()
        .any(|r| r.kind == kind && RuleLevel::of(r) == RuleLevel::default());
    if taken {
        return Err(DomainError::invalid(format!(
            "{kind} is the kind of a rule for all teams"
        )));
    }
    Ok(())
}

/// Room for one more enabled rule at `level` (`except`: the rule being
/// enabled).
fn check_room(
    rules: &[RedactionRule],
    level: &RuleLevel,
    except: Option<&str>,
) -> DomainResult<()> {
    let enabled = rules
        .iter()
        .filter(|r| r.enabled && RuleLevel::of(r) == *level && Some(r.rule_id.as_str()) != except)
        .count();
    if enabled >= MAX_RULES {
        return Err(DomainError::conflict(
            "redaction_rule_limit",
            format!("at most {MAX_RULES} enabled rules per department, team or user"),
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
            team_id: None,
            user_id: None,
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
            ("secret-field", r"x\d"),
            ("emp", "a*"),
            ("emp", "("),
        ] {
            assert!(check_rule(kind, pattern, "").is_err(), "{kind} {pattern}");
        }
        let long = "x".repeat(MAX_PATTERN_CHARS + 1);
        assert!(check_rule("emp", &long, "").is_err());
        assert!(check_rule("emp", r"x\d", &"d".repeat(201)).is_err());
    }

    struct Rules(Mutex<Vec<RedactionRule>>);

    #[async_trait::async_trait]
    impl RedactionRuleRepository for Rules {
        async fn list_redaction_rules(&self, _: &DepartmentId) -> DomainResult<Vec<RedactionRule>> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn put_redaction_rule(
            &self,
            _: &DepartmentId,
            rule: &RedactionRule,
        ) -> DomainResult<()> {
            let mut rules = self.0.lock().unwrap();
            rules.retain(|r| r.rule_id != rule.rule_id);
            rules.push(rule.clone());
            Ok(())
        }
    }

    /// Team `team-a` with admin `usr-lead` and member `usr-m`; users
    /// `usr-m`, `usr-lead`, `usr-other`.
    struct Directory;

    #[async_trait::async_trait]
    impl RedactionDirectory for Directory {
        async fn team_exists(&self, _: &DepartmentId, team_id: &str) -> DomainResult<bool> {
            Ok(team_id == "team-a")
        }
        async fn user_exists(&self, _: &DepartmentId, user_id: &str) -> DomainResult<bool> {
            Ok(["usr-m", "usr-lead", "usr-other"].contains(&user_id))
        }
        async fn active_member(
            &self,
            _: &DepartmentId,
            team_id: &str,
            user_id: &str,
        ) -> DomainResult<Option<TeamMember>> {
            use memory_domain::meta::{MemberStatus, TeamRole};
            let role = match (team_id, user_id) {
                ("team-a", "usr-lead") => TeamRole::Admin,
                ("team-a", "usr-m") => TeamRole::Member,
                _ => return Ok(None),
            };
            Ok(Some(TeamMember {
                id: format!("{team_id}:{user_id}"),
                team_id: team_id.into(),
                user_id: user_id.into(),
                role,
                status: MemberStatus::Active,
                joined_at: String::new(),
            }))
        }
    }

    fn service_with(
        deployment: DeploymentRules,
        rules: Arc<dyn RedactionRuleRepository>,
    ) -> RedactionService {
        RedactionService::new(
            deployment,
            rules,
            Arc::new(Directory),
            Arc::new(Fixed),
            Arc::new(Fixed),
        )
    }

    fn caller(user_id: &str, is_system_admin: bool) -> Caller {
        Caller {
            user_id: user_id.into(),
            is_system_admin,
        }
    }

    fn create_at(
        kind: &str,
        pattern: &str,
        team: Option<&str>,
        user: Option<&str>,
    ) -> CreateRedactionRuleInput {
        CreateRedactionRuleInput {
            kind: kind.into(),
            pattern: pattern.into(),
            description: String::new(),
            level: RuleLevel {
                team_id: team.map(Into::into),
                user_id: user.map(Into::into),
            },
        }
    }

    struct Fixed;
    impl Clock for Fixed {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::DateTime::from_timestamp(0, 0).unwrap()
        }
    }
    impl IdGenerator for Fixed {
        fn next(&self, prefix: &str) -> String {
            format!("{prefix}1")
        }
        /// Distinct per call, so each new rule gets its own id.
        fn hex(&self, n: usize) -> String {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let i = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("{i:0n$x}")
        }
        fn secret(&self, bytes: usize) -> String {
            "s".repeat(bytes)
        }
    }

    #[tokio::test]
    async fn a_kind_of_the_deployment_or_a_broad_pattern_is_refused() {
        let service = service_with(
            DeploymentRules {
                password_shape: true,
                patterns: vec![("tw-national-id".into(), r"\b[A-Z][12]\d{8}\b".into())],
            },
            Arc::new(Rules(Mutex::new(Vec::new()))),
        );
        let department = DepartmentId::parse("department", "d").unwrap();
        let admin = Caller {
            user_id: "usr-admin".into(),
            is_system_admin: true,
        };
        let create = |kind: &str, pattern: &str| create_at(kind, pattern, None, None);
        let err = service
            .create(&department, &admin, create("tw-national-id", r"\bX\d{9}\b"))
            .await
            .unwrap_err();
        assert!(err.detail.contains("deployment"), "{err:?}");
        let err = service
            .create(&department, &admin, create("words", r"\w+"))
            .await
            .unwrap_err();
        assert!(err.detail.contains("too broad"), "{err:?}");
        let member = Caller {
            user_id: "usr-m".into(),
            is_system_admin: false,
        };
        assert!(service
            .create(&department, &member, create("emp-id", r"\bEMP-\d{6}\b"))
            .await
            .is_err());
        assert!(service
            .create(&department, &admin, create("emp-id", r"\bEMP-\d{6}\b"))
            .await
            .is_ok());
    }

    /// Rules whose next read takes a snapshot, then waits until released.
    struct SlowRules {
        rules: Mutex<Vec<RedactionRule>>,
        hold: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[async_trait::async_trait]
    impl RedactionRuleRepository for SlowRules {
        async fn list_redaction_rules(&self, _: &DepartmentId) -> DomainResult<Vec<RedactionRule>> {
            let snapshot = self.rules.lock().unwrap().clone();
            // Taken out first: the lock is not held while waiting.
            let release = self.hold.lock().await.take();
            if let Some(release) = release {
                let _ = release.await;
            }
            Ok(snapshot)
        }
        async fn put_redaction_rule(
            &self,
            _: &DepartmentId,
            rule: &RedactionRule,
        ) -> DomainResult<()> {
            self.rules.lock().unwrap().push(rule.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_write_racing_a_change_does_not_cache_the_old_rules() {
        let (release, wait) = tokio::sync::oneshot::channel();
        let repo = Arc::new(SlowRules {
            rules: Mutex::new(Vec::new()),
            hold: tokio::sync::Mutex::new(Some(wait)),
        });
        let service = Arc::new(service_with(DeploymentRules::default(), repo));
        let department = DepartmentId::parse("department", "d").unwrap();
        // A write reads the rules (none yet) and is held there.
        let racing = {
            let (service, department) = (Arc::clone(&service), department.clone());
            tokio::spawn(async move {
                service
                    .redactor(&department, WriteScope::default())
                    .await
                    .unwrap()
            })
        };
        tokio::task::yield_now().await;
        // Meanwhile an admin adds a rule.
        let admin = Caller {
            user_id: "usr-admin".into(),
            is_system_admin: true,
        };
        service
            .create(
                &department,
                &admin,
                create_at("emp-id", r"\bEMP-\d{6}\b", None, None),
            )
            .await
            .unwrap();
        release.send(()).unwrap();
        let old = racing.await.unwrap();
        assert_eq!(
            old.redact("EMP-123456"),
            "EMP-123456",
            "built before the change"
        );
        // The next write gets the new rule, not the racing write's redactor.
        let next = service
            .redactor(&department, WriteScope::default())
            .await
            .unwrap();
        assert_eq!(next.redact("EMP-123456"), "[REDACTED:emp-id]");
    }

    #[test]
    fn at_most_fifty_enabled_rules_per_level() {
        let all = RuleLevel::default();
        let mut rules: Vec<_> = (0..MAX_RULES).map(|i| rule(&i.to_string(), true)).collect();
        assert!(check_room(&rules, &all, None).is_err());
        // Enabling one of them again is not one more.
        assert!(check_room(&rules, &all, Some("3")).is_ok());
        let team = RuleLevel {
            team_id: Some("team-a".into()),
            user_id: None,
        };
        assert!(
            check_room(&rules, &team, None).is_ok(),
            "another level has its own room"
        );
        rules[0].enabled = false;
        rules.push(rule("old", false));
        assert!(
            check_room(&rules, &all, None).is_ok(),
            "disabled rules do not count"
        );
    }

    #[tokio::test]
    async fn a_write_goes_through_its_team_and_user_rules_only() {
        let service = service_with(
            DeploymentRules::default(),
            Arc::new(Rules(Mutex::new(Vec::new()))),
        );
        let department = DepartmentId::parse("department", "d").unwrap();
        let admin = caller("usr-admin", true);
        for (kind, pattern, team, user) in [
            ("all-id", r"\bALL-\d{4}\b", None, None),
            ("team-id", r"\bTEAM-\d{4}\b", Some("team-a"), None),
            ("user-id", r"\bUSER-\d{4}\b", None, Some("usr-m")),
        ] {
            service
                .create(&department, &admin, create_at(kind, pattern, team, user))
                .await
                .unwrap();
        }
        let text = "ALL-1234 TEAM-1234 USER-1234";
        let redact = |scope| {
            let service = &service;
            let department = &department;
            async move {
                service
                    .redactor(department, scope)
                    .await
                    .unwrap()
                    .redact(text)
            }
        };
        assert_eq!(
            redact(WriteScope::member("team-a", "usr-m")).await,
            "[REDACTED:all-id] [REDACTED:team-id] [REDACTED:user-id]"
        );
        // Scenes and personas: no user.
        assert_eq!(
            redact(WriteScope::team("team-a")).await,
            "[REDACTED:all-id] [REDACTED:team-id] USER-1234"
        );
        assert_eq!(
            redact(WriteScope::member("team-b", "usr-other")).await,
            "[REDACTED:all-id] TEAM-1234 USER-1234"
        );
        // A user's rules follow them into every team.
        assert_eq!(
            redact(WriteScope::member("team-b", "usr-m")).await,
            "[REDACTED:all-id] TEAM-1234 [REDACTED:user-id]"
        );
    }

    #[tokio::test]
    async fn who_may_read_and_change_each_level() {
        let service = service_with(
            DeploymentRules::default(),
            Arc::new(Rules(Mutex::new(Vec::new()))),
        );
        let department = DepartmentId::parse("department", "d").unwrap();
        let (admin, lead, member, other) = (
            caller("usr-admin", true),
            caller("usr-lead", false),
            caller("usr-m", false),
            caller("usr-other", false),
        );
        let team = |kind: &str| create_at(kind, r"\bT-\d{4}\b", Some("team-a"), None);
        let mine = |kind: &str, user: &str| create_at(kind, r"\bU-\d{4}\b", None, Some(user));

        // Every team: system admins change, everyone reads.
        assert!(service
            .create(
                &department,
                &lead,
                create_at("a", r"\bA-\d{4}\b", None, None)
            )
            .await
            .is_err());
        let listing = service
            .list(&department, &other, RuleLevel::default())
            .await
            .unwrap();
        assert!(!listing.can_edit);

        // A team: its admins change, its members read, others neither.
        assert!(service
            .create(&department, &lead, team("t-lead"))
            .await
            .is_ok());
        assert!(service
            .create(&department, &admin, team("t-admin"))
            .await
            .is_ok());
        assert!(service
            .create(&department, &member, team("t-member"))
            .await
            .is_err());
        let level = RuleLevel {
            team_id: Some("team-a".into()),
            user_id: None,
        };
        let listing = service
            .list(&department, &member, level.clone())
            .await
            .unwrap();
        assert_eq!(listing.team.unwrap().len(), 2);
        assert!(!listing.can_edit);
        assert!(
            service
                .list(&department, &lead, level.clone())
                .await
                .unwrap()
                .can_edit
        );
        assert!(service.list(&department, &other, level).await.is_err());
        let missing = RuleLevel {
            team_id: Some("team-x".into()),
            user_id: None,
        };
        assert!(service.list(&department, &admin, missing).await.is_err());

        // A user: that user and system admins, nobody else.
        assert!(service
            .create(&department, &member, mine("u-self", "usr-m"))
            .await
            .is_ok());
        assert!(service
            .create(&department, &admin, mine("u-admin", "usr-m"))
            .await
            .is_ok());
        assert!(service
            .create(&department, &lead, mine("u-lead", "usr-m"))
            .await
            .is_err());
        let level = RuleLevel {
            team_id: None,
            user_id: Some("usr-m".into()),
        };
        assert!(service.list(&department, &other, level).await.is_err());

        // Changing a rule needs the right to its level.
        let rule_id = service
            .list(
                &department,
                &member,
                RuleLevel {
                    team_id: None,
                    user_id: Some("usr-m".into()),
                },
            )
            .await
            .unwrap()
            .user
            .unwrap()[0]
            .rule_id
            .clone();
        assert!(service
            .set_enabled(&department, &lead, &rule_id, false)
            .await
            .is_err());
        assert!(service
            .set_enabled(&department, &member, &rule_id, false)
            .await
            .is_ok());

        // Not both ids at once.
        let both = create_at("both", r"\bB-\d{4}\b", Some("team-a"), Some("usr-m"));
        assert!(service.create(&department, &admin, both).await.is_err());
    }

    #[tokio::test]
    async fn only_the_rules_for_all_teams_reserve_a_kind() {
        let service = service_with(
            DeploymentRules::default(),
            Arc::new(Rules(Mutex::new(Vec::new()))),
        );
        let department = DepartmentId::parse("department", "d").unwrap();
        let (admin, member, other) = (
            caller("usr-admin", true),
            caller("usr-m", false),
            caller("usr-other", false),
        );
        // A user's kind blocks nobody: not another user, a team, nor the
        // rules for all teams, and nobody learns it exists.
        service
            .create(
                &department,
                &member,
                create_at("phone", r"\bTEL-\d{6}\b", None, Some("usr-m")),
            )
            .await
            .unwrap();
        assert!(service
            .create(
                &department,
                &other,
                create_at("phone", r"\bPH-\d{6}\b", None, Some("usr-other"))
            )
            .await
            .is_ok());
        assert!(service
            .create(
                &department,
                &admin,
                create_at("phone", r"\bP-\d{6}\b", Some("team-a"), None)
            )
            .await
            .is_ok());
        assert!(service
            .create(
                &department,
                &admin,
                create_at("phone", r"\bPHONE-\d{6}\b", None, None)
            )
            .await
            .is_ok());
        // A kind of the rules for all teams, which everyone reads, is taken.
        let err = service
            .create(
                &department,
                &member,
                create_at("phone", r"\bX-\d{6}\b", None, Some("usr-m")),
            )
            .await
            .unwrap_err();
        assert!(err.detail.contains("for all teams"), "{err:?}");

        // The user's own `phone` came first: it stays editable.
        let mine = service
            .list(
                &department,
                &member,
                RuleLevel {
                    team_id: None,
                    user_id: Some("usr-m".into()),
                },
            )
            .await
            .unwrap()
            .user
            .unwrap()[0]
            .rule_id
            .clone();
        let update = |kind: Option<&str>| UpdateRedactionRuleInput {
            rule_id: mine.clone(),
            kind: kind.map(Into::into),
            pattern: None,
            description: Some("my phone".into()),
        };
        assert!(service
            .update(&department, &member, update(None))
            .await
            .is_ok());
        assert!(service
            .update(&department, &member, update(Some("phone")))
            .await
            .is_ok());
        // Renaming to a kind of the rules for all teams is still refused.
        service
            .create(
                &department,
                &admin,
                create_at("badge", r"\bB-\d{6}\b", None, None),
            )
            .await
            .unwrap();
        assert!(service
            .update(&department, &member, update(Some("badge")))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_missing_team_answers_like_a_team_the_caller_is_not_in() {
        let service = service_with(
            DeploymentRules::default(),
            Arc::new(Rules(Mutex::new(Vec::new()))),
        );
        let department = DepartmentId::parse("department", "d").unwrap();
        let other = caller("usr-other", false);
        let at = |team: &str| RuleLevel {
            team_id: Some(team.into()),
            user_id: None,
        };
        let missing = service
            .list(&department, &other, at("team-x"))
            .await
            .unwrap_err();
        let existing = service
            .list(&department, &other, at("team-a"))
            .await
            .unwrap_err();
        assert_eq!(missing.kind, existing.kind);
        assert_eq!(missing.detail, existing.detail);
    }

    #[tokio::test]
    async fn a_test_for_a_user_in_a_team_goes_through_both() {
        let service = service_with(
            DeploymentRules::default(),
            Arc::new(Rules(Mutex::new(Vec::new()))),
        );
        let department = DepartmentId::parse("department", "d").unwrap();
        let (admin, member, other) = (
            caller("usr-admin", true),
            caller("usr-m", false),
            caller("usr-other", false),
        );
        service
            .create(
                &department,
                &admin,
                create_at("team-id", r"\bTEAM-\d{4}\b", Some("team-a"), None),
            )
            .await
            .unwrap();
        service
            .create(
                &department,
                &member,
                create_at("user-id", r"\bUSER-\d{4}\b", None, Some("usr-m")),
            )
            .await
            .unwrap();
        let input = |team: Option<&str>| TestRedactionInput {
            text: "TEAM-1234 USER-1234".into(),
            pattern: None,
            kind: None,
            level: RuleLevel {
                team_id: team.map(Into::into),
                user_id: Some("usr-m".into()),
            },
        };
        let alone = service
            .test(&department, &member, input(None))
            .await
            .unwrap();
        assert_eq!(alone.redacted, "TEAM-1234 [REDACTED:user-id]");
        let in_team = service
            .test(&department, &member, input(Some("team-a")))
            .await
            .unwrap();
        assert_eq!(in_team.redacted, "[REDACTED:team-id] [REDACTED:user-id]");
        // Both levels must be readable.
        assert!(service
            .test(&department, &other, input(Some("team-a")))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn expired_redactors_do_not_pile_up() {
        let service = service_with(
            DeploymentRules::default(),
            Arc::new(Rules(Mutex::new(Vec::new()))),
        );
        let department = DepartmentId::parse("department", "d").unwrap();
        service
            .redactor(&department, WriteScope::member("team-a", "usr-m"))
            .await
            .unwrap();
        // Age the entry past the TTL.
        if let Ok(mut cache) = service.cache.lock() {
            for (_, at) in cache.values_mut() {
                *at = Instant::now() - CACHE_TTL - Duration::from_secs(1);
            }
        }
        service
            .redactor(&department, WriteScope::member("team-a", "usr-other"))
            .await
            .unwrap();
        assert_eq!(service.cache.lock().unwrap().len(), 1);
    }
}
