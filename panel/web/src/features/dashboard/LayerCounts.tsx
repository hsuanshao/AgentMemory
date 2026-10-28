import type { StatsOverview } from "../../api";
import { Card, Stat } from "../../components/Card";
import { ErrorBanner } from "../../components/Feedback";
import { useT } from "../../i18n";
import { formatNumber } from "../../lib/format";
import type { Slot } from "./useDashboardData";

export function LayerCounts({ slot }: { slot: Slot<StatsOverview> }) {
  const t = useT();
  const s = slot.data;
  return (
    <Card title={t("layers.title")}>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 xl:grid-cols-6">
        <Stat label={t("layers.l0")} value={formatNumber(s?.l0)} />
        <Stat
          label={t("layers.l1")}
          value={formatNumber(s?.l1.total)}
          tone="accent"
          hint={s ? t("layers.l1Hint", s.l1) : undefined}
        />
        <Stat label={t("layers.l2")} value={formatNumber(s?.l2)} />
        <Stat label={t("layers.l3")} value={formatNumber(s?.l3)} />
        <Stat label={t("layers.skills")} value={formatNumber(s?.skills)} />
        <Stat label={t("layers.logs")} value={formatNumber(s?.generation_logs)} />
      </div>
      {s && <L1Breakdown l1={s.l1} />}
      <ErrorBanner error={slot.error} className="mt-3" />
    </Card>
  );
}

const L1_SEGMENTS = [
  { key: "episodic", label: "episodic", color: "bg-accent" },
  { key: "persona", label: "persona", color: "bg-ok" },
  { key: "instruction", label: "instruction", color: "bg-warn" },
] as const;

function L1Breakdown({ l1 }: { l1: StatsOverview["l1"] }) {
  const total = l1.episodic + l1.persona + l1.instruction;
  if (total === 0) return null;
  return (
    <div className="mt-3">
      <div className="flex h-2 overflow-hidden rounded-sm bg-raised">
        {L1_SEGMENTS.map((seg) => (
          <div key={seg.key} className={seg.color} style={{ width: `${(l1[seg.key] / total) * 100}%` }} title={`${seg.label}: ${l1[seg.key]}`} />
        ))}
      </div>
      <div className="mt-1.5 flex gap-4 text-xxs text-muted">
        {L1_SEGMENTS.map((seg) => (
          <span key={seg.key} className="flex items-center gap-1">
            <span className={`h-2 w-2 rounded-xs ${seg.color}`} />
            {seg.label} {Math.round((l1[seg.key] / total) * 100)}%
          </span>
        ))}
      </div>
    </div>
  );
}
