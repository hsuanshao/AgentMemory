import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type CoreHealth,
  type DeadLetter,
  type GenerationLogItem,
  type GenerationSummary,
  type PipelineStatus,
  type StatsOverview,
} from "../../api";
import { usePolling } from "../../hooks/usePolling";

/** One independently-failing slot per data source. */
export interface Slot<T> {
  data?: T;
  error?: unknown;
}

export interface DashboardData {
  health: Slot<CoreHealth>;
  pipeline: Slot<PipelineStatus>;
  stats: Slot<StatsOverview>;
  summary: Slot<GenerationSummary>;
  failures: Slot<GenerationLogItem[]>;
  deadLetters: Slot<DeadLetter[]>;
  refreshedAt?: number;
  loading: boolean;
}

export const REFRESH_INTERVAL_MS = 5_000;

function settle<T>(result: PromiseSettledResult<T>, previous: Slot<T>): Slot<T> {
  // Keep the last good value on failure so cards don't flash empty.
  return result.status === "fulfilled" ? { data: result.value } : { data: previous.data, error: result.reason };
}

export function useDashboardData(paused: boolean): DashboardData & { refresh: () => Promise<void> } {
  const [state, setState] = useState<DashboardData>({
    health: {},
    pipeline: {},
    stats: {},
    summary: {},
    failures: {},
    deadLetters: {},
    loading: true,
  });
  const inFlight = useRef(false);

  const refresh = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setState((s) => ({ ...s, loading: true }));
    const [health, pipeline, stats, summary, failures, deadLetters] = await Promise.allSettled([
      api.health(),
      api.pipelineStatus(),
      api.statsOverview(),
      api.generationLog.summary(24),
      api.generationLog.list({ status: "failed", limit: 5 }).then((l) => l.items),
      api.deadLetters.list({ limit: 100 }).then((l) => l.items),
    ]);
    setState((s) => ({
      health: settle(health, s.health),
      pipeline: settle(pipeline, s.pipeline),
      stats: settle(stats, s.stats),
      summary: settle(summary, s.summary),
      failures: settle(failures, s.failures),
      deadLetters: settle(deadLetters, s.deadLetters),
      refreshedAt: Date.now(),
      loading: false,
    }));
    inFlight.current = false;
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);
  usePolling(refresh, REFRESH_INTERVAL_MS, paused);

  return { ...state, refresh };
}
