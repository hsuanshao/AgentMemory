import { useState } from "react";
import { Button } from "../../components/Button";
import { Spinner } from "../../components/Feedback";
import { PageHeader } from "../../components/PageHeader";
import { formatTime } from "../../lib/format";
import { DistillationPanel } from "./DistillationPanel";
import { HealthPanel } from "./HealthPanel";
import { LayerCounts } from "./LayerCounts";
import { PipelinePanel } from "./PipelinePanel";
import { REFRESH_INTERVAL_MS, useDashboardData } from "./useDashboardData";

export function DashboardPage() {
  const [paused, setPaused] = useState(false);
  const data = useDashboardData(paused);

  return (
    <div className="space-y-4">
      <PageHeader
        title="Dashboard"
        subtitle={
          <>
            {paused ? "Auto-refresh paused" : `Auto-refresh every ${REFRESH_INTERVAL_MS / 1000}s`}
            {data.refreshedAt && ` · updated ${formatTime(data.refreshedAt)}`}
          </>
        }
        actions={
          <>
            {data.loading && <Spinner />}
            <Button size="sm" onClick={() => setPaused((p) => !p)} aria-pressed={paused}>
              {paused ? "▶ Resume" : "❚❚ Pause"}
            </Button>
            <Button size="sm" onClick={() => void data.refresh()}>
              ↻ Refresh
            </Button>
          </>
        }
      />
      <HealthPanel slot={data.health} />
      <LayerCounts slot={data.stats} />
      <DistillationPanel
        summary={data.summary}
        failures={data.failures}
        deadLetters={data.deadLetters}
        onChanged={() => void data.refresh()}
      />
      <PipelinePanel slot={data.pipeline} />
    </div>
  );
}
