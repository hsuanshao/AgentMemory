import type { CallOptions, PanelClient } from "./client";
import type {
  Agent,
  Asset,
  AtomicDetail,
  AtomicQueryResult,
  AtomicType,
  AtomicUpdateResult,
  AuthVerifyResult,
  BatchDeleteResult,
  CreatedUser,
  ConversationItem,
  ConversationQueryResult,
  CoreHealth,
  CoreMemory,
  GenerationLayer,
  GenerationLog,
  GenerationLogList,
  GenerationStatus,
  GenerationSummary,
  DeadLetter,
  JobKind,
  MetaList,
  Page,
  PanelConfig,
  PipelineStatus,
  RecallResult,
  ScenarioFile,
  ScenarioList,
  Scope,
  SkillDetail,
  SkillSearchHit,
  SkillSummary,
  SkillVersion,
  StatsOverview,
  Team,
  TeamMember,
  TeamRole,
  UserKeyInfo,
  UserPublic,
  WriteResult,
} from "./types";

/**
 * One typed function per Core v3 endpoint the panel uses. Every v3 path here
 * must also be in the BFF allowlist (panel/server/src/http/allowlist.ts).
 */
export function createCoreApi(client: PanelClient) {
  const call = <T>(path: string, body: object, opts?: CallOptions) => client.core<T>(path, body, opts);

  return {
    panelConfig: (opts?: CallOptions) => client.getJson<PanelConfig>("/api/config", opts),
    health: (opts?: CallOptions) => client.getJson<CoreHealth>("/api/core-health", opts),
    pipelineStatus: (opts?: CallOptions) => call<PipelineStatus>("pipeline/status", {}, opts),
    deadLetters: {
      list: (filter: { kind?: JobKind; limit?: number } = {}, opts?: CallOptions) =>
        call<{ items: DeadLetter[] }>("pipeline/dead-letter/list", filter, opts),
      /** Requeues the oldest matching dead letters; system admins only. */
      redrive: (filter: { kind?: JobKind; limit?: number }) =>
        call<{ requeued: number }>("pipeline/dead-letter/redrive", filter),
    },
    statsOverview: (opts?: CallOptions) => call<StatsOverview>("stats/overview", {}, opts),

    verifyUserKey: (userKey: string) => call<AuthVerifyResult>("meta/auth/verify", { user_key: userKey }),

    recall: (scope: Scope, query: string, maxResults?: number) =>
      call<RecallResult>("recall", { ...scope, query, max_results: maxResults }),

    conversation: {
      query: (scope: Scope, page: Page & { session_id?: string }) =>
        call<ConversationQueryResult>("conversation/query", { ...scope, ...page }),
      search: (scope: Scope, query: string, limit = 20) =>
        call<{ messages: ConversationItem[] }>("conversation/search", { ...scope, query, limit }),
      delete: (scope: Scope, messageIds: string[]) =>
        call<{ deleted_count: number }>("conversation/delete", { ...scope, message_ids: messageIds }),
    },

    atomic: {
      query: (scope: Scope, page: Page & { type?: AtomicType }) =>
        call<AtomicQueryResult>("atomic/query", { ...scope, ...page }),
      search: (scope: Scope, query: string, type?: AtomicType, limit = 20) =>
        call<{ items: AtomicDetail[] }>("atomic/search", { ...scope, query, type, limit }),
      update: (scope: Scope, id: string, content: string, background?: string) =>
        call<AtomicUpdateResult>("atomic/update", { ...scope, id, content, background }),
      delete: (scope: Scope, ids: string[]) => call<{ deleted_count: number }>("atomic/delete", { ...scope, ids }),
    },

    scenario: {
      ls: (scope: Scope, pathPrefix?: string) => call<ScenarioList>("scenario/ls", { ...scope, path_prefix: pathPrefix }),
      read: (scope: Scope, path: string) => call<ScenarioFile>("scenario/read", { ...scope, path }),
      write: (scope: Scope, path: string, content: string, summary?: string) =>
        call<WriteResult & { path: string }>("scenario/write", { ...scope, path, content, summary }),
    },

    core: {
      read: (scope: Scope) => call<CoreMemory>("core/read", { ...scope }),
      write: (scope: Scope, content: string) => call<WriteResult>("core/write", { ...scope, content }),
    },

    generationLog: {
      list: (filter: { layer?: GenerationLayer; status?: GenerationStatus; limit?: number; cursor?: string }) =>
        call<GenerationLogList>("memory-generation-log/list", filter),
      get: (logId: string) => call<GenerationLog>("memory-generation-log/get", { log_id: logId }),
      summary: (hours = 24, opts?: CallOptions) =>
        call<GenerationSummary>("memory-generation-log/summary", { hours }, opts),
    },

    skill: {
      list: (scope: Scope, filters: { name_prefix?: string; status?: string[] }, page: Page) =>
        call<{ items: SkillSummary[]; total: number }>("skill/list", { ...scope, filters, pagination: page }),
      search: (scope: Scope, query: string, topK = 20) =>
        call<{ items: SkillSearchHit[] }>("skill/search", { ...scope, query, top_k: topK, scope: "team" }),
      get: (scope: Scope, skillId: string, version?: number) =>
        call<SkillDetail>("skill/get", { ...scope, skill_id: skillId, version, include_content: true }),
      versions: (scope: Scope, skillId: string) =>
        call<{ items: SkillVersion[]; total: number }>("skill/versions", { ...scope, skill_id: skillId }),
    },

    meta: {
      teams: (userId: string, page: Page = {}) => call<MetaList<Team>>("meta/team/list", { user_id: userId, ...page }),
      members: (teamId: string, page: Page = {}) =>
        call<MetaList<TeamMember>>("meta/team-member/list", { team_id: teamId, ...page }),
      agents: (teamId: string, page: Page = {}) => call<MetaList<Agent>>("meta/agent/list", { team_id: teamId, ...page }),
      accessibleAssets: (userId: string, filter: { team_id?: string; asset_type?: string } & Page = {}) =>
        call<MetaList<Asset>>("meta/asset/list-accessible", { user_id: userId, ...filter }),
    },

    /** System-admin operations; the BFF refuses them for everyone else. */
    admin: {
      allTeams: (page: Page = {}) => call<MetaList<Team>>("meta/team/list", { all: true, ...page }),
      createTeam: (input: { name: string; owner_user_id: string; description?: string }) => call<Team>("meta/team/create", input),
      updateTeam: (input: { team_id: string; name?: string; description?: string; status?: string }) =>
        call<Team>("meta/team/update", input),
      deleteTeam: (teamId: string) => call<BatchDeleteResult>("meta/team/delete", { team_ids: [teamId] }),

      addMember: (teamId: string, userId: string, role: TeamRole) =>
        call<TeamMember>("meta/team-member/add", { team_id: teamId, user_id: userId, role }),
      removeMember: (teamId: string, userId: string) =>
        call<unknown>("meta/team-member/remove", { team_id: teamId, user_id: userId }),

      createAgent: (input: { team_id: string; owner_user_id: string; name: string; description?: string }) =>
        call<Agent>("meta/agent/create", input),
      deleteAgent: (agentId: string) => call<BatchDeleteResult>("meta/agent/delete", { agent_ids: [agentId] }),

      users: (page: Page = {}) => call<MetaList<UserPublic>>("meta/user/list", page),
      createUser: (username: string) => call<CreatedUser>("meta/user/create", { username }),
      deleteUser: (userId: string) => call<BatchDeleteResult>("meta/user/delete", { user_ids: [userId] }),

      keys: (userId: string, page: Page = {}) => call<MetaList<UserKeyInfo>>("meta/user-key/list", { user_id: userId, ...page }),
      createKey: (userId: string, name?: string) => call<UserKeyInfo>("meta/user-key/create", { user_id: userId, name }),
      revokeKey: (keyId: string) => call<unknown>("meta/user-key/revoke", { key_id: keyId }),
    },
  };
}

export type CoreApi = ReturnType<typeof createCoreApi>;
