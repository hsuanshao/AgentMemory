import { useState } from "react";
import { Button } from "../../components/Button";
import { Spinner } from "../../components/Feedback";
import { PageHeader } from "../../components/PageHeader";
import { useT } from "../../i18n";
import { formatTime } from "../../lib/format";
import { DistillationPanel } from "./DistillationPanel";
import { HealthPanel } from "./HealthPanel";
import { LayerCounts } from "./LayerCounts";
import { PipelinePanel } from "./PipelinePanel";
import { REFRESH_INTERVAL_MS, useDashboardData } from "./useDashboardData";

export function DashboardPage() {
  const t = useT();
  const [paused, setPaused] = useState(false);
  const data = useDashboardData(paused);

  return (
    <div className="space-y-4">
      <PageHeader
        title={t("dashboard.title")}
        subtitle={
          <>
            {paused ? t("dashboard.paused") : t("dashboard.every", { seconds: REFRESH_INTERVAL_MS / 1000 })}
            {data.refreshedAt && t("dashboard.updatedAt", { time: formatTime(data.refreshedAt) })}
          </>
        }
        actions={
          <>
            {data.loading && <Spinner />}
            <Button size="sm" onClick={() => setPaused((p) => !p)} aria-pressed={paused}>
              {paused ? t("dashboard.resume") : t("dashboard.pause")}
            </Button>
            <Button size="sm" onClick={() => void data.refresh()}>
              {t("common.refresh")}
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
