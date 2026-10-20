import type { Scope } from "../../api";
import { createPersistedStore } from "../../lib/store";

/** Empty until the picker selects a real team and agent (Core has no default bucket). */
export const DEFAULT_SCOPE: Scope = { team_id: "", agent_id: "", user_id: "" };

export function isComplete(scope: Scope): boolean {
  return !!scope.team_id && !!scope.agent_id;
}

function isScope(v: unknown): v is Scope {
  if (typeof v !== "object" || v === null) return false;
  const s = v as Record<string, unknown>;
  return typeof s.team_id === "string" && typeof s.agent_id === "string" && typeof s.user_id === "string";
}

/** Memory-browser isolation scope, remembered per browser. */
export const scopeStore = createPersistedStore<Scope>("memory-panel.scope", DEFAULT_SCOPE, isScope);

export type MemoryTab = "l0" | "l1" | "l2" | "l3" | "recall";

/** `/memory?team=…&agent=…[&user=…][&tab=…]`; an empty user means the caller. */
export function memoryLink(scope: Scope, tab: MemoryTab): string {
  const params = new URLSearchParams({ team: scope.team_id, agent: scope.agent_id });
  if (scope.user_id) params.set("user", scope.user_id);
  params.set("tab", tab);
  return `/memory?${params}`;
}

/** The scope and tab a memory link names, or nothing when it names no scope. */
export function fromLink(params: URLSearchParams): { scope: Scope; tab?: MemoryTab } | undefined {
  const team_id = params.get("team") ?? "";
  const agent_id = params.get("agent") ?? "";
  if (!team_id || !agent_id) return undefined;
  const tab = params.get("tab");
  const tabs: ReadonlyArray<string> = ["l0", "l1", "l2", "l3", "recall"];
  return {
    scope: { team_id, agent_id, user_id: params.get("user") ?? "" },
    tab: tab && tabs.includes(tab) ? (tab as MemoryTab) : undefined,
  };
}

export function scopeKey(scope: Scope): string {
  return `${scope.team_id}/${scope.agent_id}/${scope.user_id}`;
}
