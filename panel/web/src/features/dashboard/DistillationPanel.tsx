import { useState } from "react";
import { Link } from "react-router-dom";
import { api, type DeadLetter, type GenerationLogItem, type GenerationSummary, type JobKind } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card, Stat } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Field, Select } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { countByKind, deadLetterSubject, JOB_KINDS, layerRows, type LayerRow } from "../../lib/distillation";
import { formatNumber, formatRelative, formatTime } from "../../lib/format";
import { useT, withCount, type Translate } from "../../i18n";
import { useIsAdmin } from "../auth/session";
import type { Slot } from "./useDashboardData";

const REDRIVE_BATCH = 10;

/**
 * How distillation is doing: which LLM runs it, runs per layer in the last
 * day, the latest failures, and the jobs that failed every retry.
 */
export function DistillationPanel({
  summary,
  failures,
  deadLetters,
  onChanged,
}: {
  summary: Slot<GenerationSummary>;
  failures: Slot<GenerationLogItem[]>;
  deadLetters: Slot<DeadLetter[]>;
  onChanged: () => void;
}) {
  const t = useT();
  const s = summary.data;
  const rows = layerRows(s);
  return (
    <>
      <div className="grid gap-4 xl:grid-cols-[2fr_1fr]">
        <Card
          title={t("distill.title", { hours: s?.hours ?? 24 })}
          actions={
            s && (
              <span className="flex items-center gap-2">
                <span className="label">LLM</span>
                <Badge tone={s.llm.configured ? "accent" : "err"}>{s.llm.configured ? s.llm.model : t("distill.notConfigured")}</Badge>
              </span>
            )
          }
          bodyClassName="p-0"
        >
          <DataTable columns={layerColumns(t)} rows={rows} rowKey={(r) => r.layer} loading={!s && !summary.error} />
          <LatestFailures slot={failures} />
        </Card>
        <DeadLetterCard slot={deadLetters} onChanged={onChanged} />
      </div>
      <ErrorBanner error={summary.error ?? failures.error ?? deadLetters.error} />
    </>
  );
}

const layerColumns = (t: Translate): ReadonlyArray<Column<LayerRow>> => [
  { key: "layer", header: t("common.layer"), render: (r) => <Badge>{r.layer}</Badge> },
  { key: "ok", header: t("distill.succeeded"), className: "text-right", render: (r) => <span className="mono">{formatNumber(r.succeeded)}</span> },
  {
    key: "empty",
    header: t("distill.empty"),
    className: "text-right",
    render: (r) => <span className="mono text-muted" title={t("distill.emptyHint")}>{formatNumber(r.empty)}</span>,
  },
  {
    key: "failed",
    header: t("distill.failed"),
    className: "text-right",
    render: (r) => <span className={r.failed ? "mono text-err" : "mono"}>{formatNumber(r.failed)}</span>,
  },
  {
    key: "dropped",
    header: t("distill.dropped"),
    className: "text-right",
    render: (r) => (
      <span className={r.dropped ? "mono text-warn" : "mono"} title={t("distill.droppedHint")}>
        {formatNumber(r.dropped)}
      </span>
    ),
  },
  {
    key: "rate",
    header: t("distill.failureRate"),
    className: "text-right",
    render: (r) =>
      r.failureRate === undefined ? (
        <span className="text-muted">—</span>
      ) : (
        <span className={r.failureRate > 0 ? "mono text-err" : "mono"}>{Math.round(r.failureRate * 100)}%</span>
      ),
  },
];

function LatestFailures({ slot }: { slot: Slot<GenerationLogItem[]> }) {
  const t = useT();
  const items = slot.data ?? [];
  return (
    <div className="border-t border-line p-3">
      <div className="mb-1 flex items-center justify-between">
        <span className="label">{t("distill.latestFailures")}</span>
        <span className="flex gap-3">
          <Link to="/logs?status=dropped" className="text-xxs text-accent hover:underline">
            {t("distill.droppedLink")}
          </Link>
          <Link to="/logs?status=failed" className="text-xxs text-accent hover:underline">
            {t("distill.failedLink")}
          </Link>
        </span>
      </div>
      {items.length === 0 ? (
        <div className="text-muted">{slot.data ? t("distill.noFailures") : "…"}</div>
      ) : (
        <ul className="space-y-1.5">
          {items.map((f) => (
            <li key={f.log_id} className="grid grid-cols-[auto_auto_1fr] items-start gap-2">
              <span className="mono whitespace-nowrap text-muted" title={formatTime(f.finished_at_ms)}>
                {formatRelative(f.finished_at_ms)}
              </span>
              <Badge>{f.layer}</Badge>
              <span className="mono break-all text-err" title={f.error_message ?? undefined}>
                {oneLine(f.error_message ?? t("distill.failedFallback"), 180)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function DeadLetterCard({ slot, onChanged }: { slot: Slot<DeadLetter[]>; onChanged: () => void }) {
  const t = useT();
  const admin = useIsAdmin();
  const [kind, setKind] = useState<JobKind | "">("");
  const [result, setResult] = useState<string>();
  const letters = slot.data ?? [];
  const counts = countByKind(letters);
  const redrive = useAction(async () => {
    const { requeued } = await api.deadLetters.redrive({ kind: kind || undefined, limit: REDRIVE_BATCH });
    setResult(t("dead.requeued", { count: requeued }));
    onChanged();
  });

  return (
    <Card title={withCount(t("dead.title"), slot.data ? letters.length : undefined)} bodyClassName="p-0">
      <div className="grid grid-cols-4 gap-2 p-3">
        {JOB_KINDS.map((k) => (
          <Stat key={k} label={k} value={formatNumber(counts[k])} tone={counts[k] ? "err" : "default"} />
        ))}
      </div>
      {letters.length > 0 && (
        <DataTable columns={deadColumns(t)} rows={letters.slice(0, 8)} rowKey={(d) => `${d.dead_at_ms}:${JSON.stringify(d.job)}`} />
      )}
      {admin && letters.length > 0 && (
        <div className="flex flex-wrap items-end gap-2 border-t border-line p-3">
          <Field label={t("common.kind")}>
            <Select
              options={[{ value: "", label: t("dead.any") }, ...JOB_KINDS.map((k) => ({ value: k, label: k }))]}
              value={kind}
              onChange={(e) => setKind(e.target.value as JobKind | "")}
            />
          </Field>
          <Button size="sm" busy={redrive.busy} onClick={() => void redrive.run()} title={t("dead.rerunHint")}>
            {t("dead.rerun", { count: REDRIVE_BATCH })}
          </Button>
          {result && <span className="text-muted">{result}</span>}
        </div>
      )}
      <ErrorBanner error={redrive.error} />
    </Card>
  );
}

const deadColumns = (t: Translate): ReadonlyArray<Column<DeadLetter>> => [
  {
    key: "when",
    header: t("dead.when"),
    render: (d) => (
      <span className="mono whitespace-nowrap text-muted" title={d.dead_at_ms ? formatTime(d.dead_at_ms) : undefined}>
        {d.dead_at_ms ? formatRelative(d.dead_at_ms) : "—"}
      </span>
    ),
  },
  { key: "kind", header: t("common.kind"), render: (d) => <Badge tone="err">{d.kind}</Badge> },
  {
    key: "redrives",
    header: t("dead.reruns"),
    className: "text-right",
    render: (d) => (
      <span className="mono text-muted" title={t("dead.rerunsHint")}>
        {d.redrives ?? 0}
      </span>
    ),
  },
  {
    key: "what",
    header: t("dead.job"),
    render: (d) => (
      <div className="mono">
        <div className="break-all">{deadLetterSubject(d)}</div>
        {d.error && (
          <div className="break-all text-err" title={d.error}>
            {oneLine(d.error, 120)}
          </div>
        )}
      </div>
    ),
  },
];

function oneLine(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}
