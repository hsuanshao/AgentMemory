/**
 * Wire types for the Core v3 endpoints the panel uses. Field shapes follow
 * docs/API.md and the reference v3 API document. Optional fields are those
 * Core may omit.
 */

export interface Envelope<T> {
  code: number;
  message: string;
  request_id?: string;
  data?: T;
}

export interface Scope {
  team_id: string;
  agent_id: string;
  user_id: string;
}

export interface Page {
  limit?: number;
  offset?: number;
}

export interface MetaList<T> {
  items: T[];
  total: number;
  limit: number;
  offset: number;
}

// ---------- panel / health ----------

export interface PanelConfig {
  department_id: string;
  core_url_display: string;
}

export interface CoreHealth {
  status: string;
  version?: string;
  uptime?: number;
  stores?: {
    vectorStore?: boolean;
    embeddingService?: boolean;
    ftsAvailable?: boolean;
    llmConfigured?: boolean;
  };
  services?: Record<string, unknown>;
}

export interface WorkerMetrics {
  tasks_enqueued: number;
  tasks_completed: number;
  tasks_failed: number;
  tasks_retried: number;
  tasks_dead_lettered: number;
  last_task_at?: string | null;
  last_error?: string | null;
}

export interface ScopeState {
  scope: string;
  team_id?: string;
  agent_id?: string;
  user_id?: string;
  session_id?: string;
  conversation_count: number;
  warmup_threshold: number;
  l1_cursor?: string | number | null;
  l2_pending: number;
  last_l1_at?: string | null;
  last_l2_at?: string | null;
  last_l3_at?: string | null;
  memories_since_persona: number;
}

export interface PipelineStatus {
  worker: WorkerMetrics;
  queue: { pending: number; running: number; dead_letter: number };
  scopes: ScopeState[];
}

export interface StatsOverview {
  l0: number;
  l1: { total: number; episodic: number; persona: number; instruction: number };
  l2: number;
  l3: number;
  skills: number;
  generation_logs: number;
}

// ---------- auth ----------

export type UserType = "normal" | "system_admin";

export interface UserPublic {
  user_id: string;
  user_type: UserType;
  username: string;
  created_at: string;
}

export interface AuthVerifyResult {
  valid: boolean;
  user: UserPublic | null;
}

// ---------- L0 ----------

export interface ConversationItem {
  id: string;
  version?: string | number;
  role: "user" | "assistant" | string;
  content: string;
  timestamp?: string | number | null;
  recorded_at?: string | null;
  session_id?: string | null;
  score?: number;
}

export interface ConversationQueryResult {
  messages: ConversationItem[];
  total: number;
}

// ---------- L1 ----------

export type AtomicType = "episodic" | "persona" | "instruction";
export const ATOMIC_TYPES: readonly AtomicType[] = ["episodic", "persona", "instruction"];

export interface AtomicDetail {
  id: string;
  version: number | string;
  type: AtomicType;
  background?: string | null;
  content: string;
  created_at: string;
  updated_at: string;
  score?: number;
}

export interface AtomicQueryResult {
  items: AtomicDetail[];
  total?: number;
}

export interface AtomicUpdateResult {
  id: string;
  version: string | number;
  updated_at: string;
}

// ---------- L2 / L3 ----------

export interface ScenarioEntry {
  path: string;
  summary?: string | null;
  version: number;
  created_at?: string;
  updated_at?: string;
}

export interface ScenarioList {
  entries: ScenarioEntry[];
  total: number;
}

export interface ScenarioFile {
  path: string;
  version?: number | null;
  content: string | null;
  created_at: string | null;
  updated_at: string | null;
}

export interface CoreMemory {
  content: string | null;
  version?: number | null;
  created_at?: string | null;
  updated_at?: string | null;
}

export interface WriteResult {
  version: number | string;
  updated_at: string;
}

// ---------- recall ----------

export interface RecallMemory {
  id: string;
  type: string;
  content: string;
  score: number;
}

export interface RecallResult {
  strategy: string;
  memories: RecallMemory[];
  prepend_context?: string | null;
  append_system_context?: string | null;
}

// ---------- generation logs ----------

export type GenerationLayer = "l1" | "l2" | "l3";
/** `empty`: the LLM answered but nothing was written. */
export type GenerationStatus = "succeeded" | "empty" | "failed";

export interface GenerationLogItem {
  log_id: string;
  layer: GenerationLayer;
  status: GenerationStatus;
  team_id?: string;
  agent_id?: string;
  user_id?: string | null;
  session_id?: string | null;
  model?: string;
  started_at_ms?: number;
  finished_at_ms: number;
  latency_ms?: number;
  error_message?: string | null;
}

export interface GenerationLogList {
  items: GenerationLogItem[];
  next_cursor?: string | null;
}

export interface GenerationRef {
  layer: string;
  record_id: string;
}

export interface GenerationLog extends GenerationLogItem {
  memory_prompt_id?: string;
  prompt_sha256?: string;
  input_refs?: GenerationRef[];
  output_refs?: GenerationRef[];
  [extra: string]: unknown;
}

export interface GenerationSummary {
  hours: number;
  from_ms: number;
  /** The LLM distillation uses, as generation logs record it. */
  llm: { configured: boolean; model: string };
  items: { layer: GenerationLayer; status: GenerationStatus; count: number }[];
}

// ---------- dead letters ----------

export type JobKind = "l1" | "l2" | "l3" | "skill";

/** A pipeline job that failed every retry. `job` is `{L1: {...}}` and so on. */
export interface DeadLetter {
  kind: JobKind;
  job: Record<string, Record<string, unknown>>;
  attempt: number;
  error?: string | null;
  /** 0 when written before the time was recorded. */
  dead_at_ms: number;
}

// ---------- skills ----------

export interface SkillSummary {
  skill_id: string;
  name: string;
  description?: string | null;
  version: number;
  is_head?: boolean;
  status: "active" | "archived" | string;
  owner_user_id: string;
  owner_agent_id: string;
  team_id?: string | null;
  created_at_ms?: number;
  updated_at_ms?: number;
  metadata?: Record<string, unknown> | null;
}

export interface SkillSearchHit extends SkillSummary {
  score: number;
  snippet?: string;
}

export interface SkillDetail extends SkillSummary {
  content?: string;
  manifest?: unknown;
  content_hash?: string;
}

export interface SkillVersion extends SkillSummary {
  is_expired?: boolean;
}

// ---------- metadata ----------

export interface Team {
  team_id: string;
  name: string;
  description?: string | null;
  owner_user_id: string;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface TeamMember {
  id: string;
  team_id: string;
  user_id: string;
  role: string;
  joined_at: string;
  status: string;
  username?: string;
}

export type TeamRole = "admin" | "member" | "reviewer";

export interface UserKeyInfo {
  key_id: string;
  user_id: string;
  /** Masked key value. */
  key_prefix: string;
  name?: string | null;
  status: string;
  is_default: boolean;
  last_used_at?: string | null;
  expires_at?: string | null;
  created_at: string;
  /** Only present in the response that created the key. */
  key_value?: string;
}

export interface CreatedUser {
  user_id: string;
  user_type: UserType;
  username: string;
  created_at: string;
  default_user_key: string;
}

export interface BatchDeleteResult {
  deleted_ids: string[];
  failed: { id: string; reason: string }[];
}

export interface Agent {
  agent_id: string;
  team_id: string;
  owner_user_id: string;
  name: string;
  description?: string | null;
  visibility: string;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface Asset {
  asset_id: string;
  team_id: string;
  asset_type: string;
  name: string;
  description?: string | null;
  owner_user_id: string;
  source_type: string;
  version: number;
  visibility: string;
  status: string;
  usage_count: number;
  last_used_at?: string | null;
  updated_at: string;
}
