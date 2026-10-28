/**
 * Explicit allowlist of Core `/v3/*` paths the panel UI may call through the
 * BFF. Anything else is rejected with 404 so the BFF (which holds the Core
 * API key) never becomes an open proxy for destructive operations such as
 * `department/destroy` or `internal/meta/*`.
 */
export const CORE_V3_ALLOWLIST: ReadonlySet<string> = new Set([
  // monitoring
  "pipeline/status",
  "pipeline/dead-letter/list",
  "pipeline/dead-letter/redrive",
  "stats/overview",
  // recall test
  "recall",
  // L0
  "conversation/query",
  "conversation/search",
  "conversation/delete",
  // L1
  "atomic/query",
  "atomic/search",
  "atomic/update",
  "atomic/delete",
  // L2
  "scenario/ls",
  "scenario/read",
  "scenario/write",
  // L3
  "core/read",
  "core/write",
  // generation logs
  "memory-generation-log/list",
  "memory-generation-log/get",
  "memory-generation-log/summary",
  // skills
  "skill/list",
  "skill/search",
  "skill/get",
  "skill/versions",
  // proposals from skill extraction (Core lets the conversation's user,
  // team admins and system admins review them, by the memory rules)
  "skill/proposal/list",
  "skill/proposal/approve",
  "skill/proposal/reject",
  // metadata
  "meta/auth/verify",
  "meta/team/list",
  "meta/team-member/list",
  "meta/agent/list",
  "meta/asset/list-accessible",
  // administration (system admins only, see ADMIN_ONLY_V3_PATHS)
  "meta/user/list",
  "meta/user/create",
  "meta/user/delete",
  "meta/user-key/list",
  "meta/user-key/create",
  "meta/user-key/revoke",
  "meta/team/create",
  "meta/team/update",
  "meta/team/delete",
  "meta/team-member/add",
  "meta/team-member/remove",
  "meta/agent/create",
  "meta/agent/update",
  "meta/agent/delete",
  // redaction rules for every team, a team or a user (Core checks who may
  // read and change each level)
  "meta/redaction-rule/list",
  "meta/redaction-rule/create",
  "meta/redaction-rule/update",
  "meta/redaction-rule/disable",
  "meta/redaction-rule/enable",
  "meta/redaction-rule/test",
  // the department's credential audit log (Core: system admins only)
  "audit/list",
]);

/** Paths callable without a verified user key (the login call itself). */
export const PUBLIC_V3_PATHS: ReadonlySet<string> = new Set(["meta/auth/verify"]);

/**
 * Every call that changes data or administers the department. In the panel
 * only system admins configure; everyone else gets a read-only view. Core
 * applies its own (finer) rules on top. Redaction rules are not here: team
 * admins manage their team's and every user their own, so Core decides.
 */
export const ADMIN_ONLY_V3_PATHS: ReadonlySet<string> = new Set([
  "pipeline/dead-letter/redrive",
  "conversation/delete",
  "atomic/update",
  "atomic/delete",
  "scenario/write",
  "core/write",
  "meta/user/list",
  "meta/user/create",
  "meta/user/delete",
  "meta/user-key/list",
  "meta/user-key/create",
  "meta/user-key/revoke",
  "meta/team/create",
  "meta/team/update",
  "meta/team/delete",
  "meta/team-member/add",
  "meta/team-member/remove",
  "meta/agent/create",
  "meta/agent/update",
  "meta/agent/delete",
  "audit/list",
]);

const SAFE_SEGMENT = /^[a-z0-9-]+$/;

/**
 * Turns the wildcard tail of `/api/core/<tail>` into a Core v3 path, or
 * `undefined` when it is malformed or not allowlisted.
 */
export function resolveV3Path(tail: string, allowlist: ReadonlySet<string> = CORE_V3_ALLOWLIST): string | undefined {
  const segments = tail.split("/").filter((s) => s.length > 0);
  if (segments.length === 0 || !segments.every((s) => SAFE_SEGMENT.test(s))) return undefined;
  const path = segments.join("/");
  return allowlist.has(path) ? path : undefined;
}
