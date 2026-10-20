import type { DeadLetter, GenerationLayer, GenerationStatus, GenerationSummary, JobKind } from "../api/types";
import type { BadgeTone } from "../components/Badge";

export const GENERATION_LAYERS: readonly GenerationLayer[] = ["l1", "l2", "l3"];
export const JOB_KINDS: readonly JobKind[] = ["l1", "l2", "l3", "skill"];

export function statusTone(status: GenerationStatus | string): BadgeTone {
  if (status === "succeeded") return "ok";
  if (status === "failed") return "err";
  return "neutral";
}

export interface LayerRow {
  layer: GenerationLayer;
  succeeded: number;
  empty: number;
  failed: number;
  /** Failed share of all runs, 0–1; undefined without runs. */
  failureRate?: number;
}

/** One row per layer, zero-filled, from a generation-log summary. */
export function layerRows(summary: GenerationSummary | undefined): LayerRow[] {
  return GENERATION_LAYERS.map((layer) => {
    const count = (status: GenerationStatus) =>
      summary?.items.filter((i) => i.layer === layer && i.status === status).reduce((n, i) => n + i.count, 0) ?? 0;
    const row = { layer, succeeded: count("succeeded"), empty: count("empty"), failed: count("failed") };
    const total = row.succeeded + row.empty + row.failed;
    return { ...row, failureRate: total ? row.failed / total : undefined };
  });
}

/** What a dead job was about: its session, skill task or agent profile. */
export function deadLetterSubject(letter: DeadLetter): string {
  const job = Object.values(letter.job)[0] ?? {};
  if (typeof job.session_id === "string") return `session ${job.session_id}`;
  if (typeof job.task_id === "string") return `task ${job.task_id}`;
  const profile = job.profile as { team_id?: string; agent_id?: string } | undefined;
  if (profile?.agent_id) return `agent ${profile.agent_id}`;
  return "—";
}

/** Dead letters per job kind, every kind present. */
export function countByKind(letters: readonly DeadLetter[]): Record<JobKind, number> {
  const counts = { l1: 0, l2: 0, l3: 0, skill: 0 } as Record<JobKind, number>;
  for (const l of letters) if (l.kind in counts) counts[l.kind] += 1;
  return counts;
}
