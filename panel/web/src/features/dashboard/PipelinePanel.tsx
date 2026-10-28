import { Link } from "react-router-dom";
import type { PipelineStatus, ScopeState } from "../../api";
import { Badge } from "../../components/Badge";
import { Card, Stat } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { formatNumber, formatRelative, formatTime } from "../../lib/format";
import { useT, withCount, type Translate } from "../../i18n";
import { memoryLink } from "../memory/scope";
import type { Slot } from "./useDashboardData";

export function PipelinePanel({ slot }: { slot: Slot<PipelineStatus> }) {
  const t = useT();
  const p = slot.data;
  return (
    <>
      <div className="grid gap-4 xl:grid-cols-[2fr_1fr]">
        <Card title={t("pipeline.worker")}>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
            <Stat label={t("pipeline.enqueued")} value={formatNumber(p?.worker.tasks_enqueued)} />
            <Stat label={t("pipeline.completed")} value={formatNumber(p?.worker.tasks_completed)} tone="ok" />
            <Stat label={t("pipeline.failed")} value={formatNumber(p?.worker.tasks_failed)} tone={p?.worker.tasks_failed ? "err" : "default"} />
            <Stat label={t("pipeline.retried")} value={formatNumber(p?.worker.tasks_retried)} tone={p?.worker.tasks_retried ? "warn" : "default"} />
            <Stat
              label={t("pipeline.deadLettered")}
              value={formatNumber(p?.worker.tasks_dead_lettered)}
              tone={p?.worker.tasks_dead_lettered ? "err" : "default"}
            />
            <Stat label={t("pipeline.lastTask")} value={formatRelative(p?.worker.last_task_at)} hint={formatTime(p?.worker.last_task_at)} />
          </div>
          {p?.worker.last_error && (
            <div className="mono mt-3 rounded-sm border border-err/30 bg-err/5 px-2 py-1.5 text-err">{t("pipeline.lastError", { error: p.worker.last_error })}</div>
          )}
        </Card>
        <Card title={t("pipeline.queue")}>
          <div className="grid grid-cols-3 gap-2">
            <Stat label={t("pipeline.pending")} value={formatNumber(p?.queue.pending)} tone={p?.queue.pending ? "warn" : "default"} />
            <Stat label={t("pipeline.running")} value={formatNumber(p?.queue.running)} tone={p?.queue.running ? "accent" : "default"} />
            <Stat label={t("pipeline.deadLetter")} value={formatNumber(p?.queue.dead_letter)} tone={p?.queue.dead_letter ? "err" : "default"} />
          </div>
        </Card>
      </div>
      <Card title={withCount(t("pipeline.scopes"), p?.scopes.length)} bodyClassName="p-0">
        <DataTable columns={scopeColumns(t)} rows={p?.scopes} rowKey={(s) => s.key} loading={!p && !slot.error} empty={t("pipeline.noScopes")} />
      </Card>
      <ErrorBanner error={slot.error} />
    </>
  );
}

const scopeColumns = (t: Translate): ReadonlyArray<Column<ScopeState>> => [
  { key: "team", header: t("common.team"), render: (s) => <Named name={s.team_name} id={s.team_id} /> },
  { key: "agent", header: t("common.agent"), render: (s) => <Named name={s.agent_name} id={s.agent_id} /> },
  { key: "user", header: t("common.user"), render: (s) => (s.user_id ? <Named name={s.username} id={s.user_id} /> : <Shared />) },
  {
    key: "session",
    header: t("common.session"),
    render: (s) => <span className="mono text-muted">{s.session_id ?? "—"}</span>,
  },
  { key: "open", header: "", render: (s) => <OpenLink row={s} /> },
  {
    key: "conv",
    header: t("pipeline.conversations"),
    className: "text-right",
    render: (s) => (
      <span className="mono">
        {s.conversation_count ?? "—"}
        {s.warmup_threshold != null && <span className="text-muted"> / {s.warmup_threshold}</span>}
      </span>
    ),
  },
  { key: "cursor", header: t("pipeline.l1Cursor"), render: (s) => <span className="mono">{s.l1_cursor ?? "—"}</span> },
  {
    key: "l2p",
    header: t("pipeline.l2Pending"),
    className: "text-right",
    render: (s) => (s.l2_pending ? <Badge tone="warn">{t("pipeline.pendingBadge")}</Badge> : <span className="mono text-muted">—</span>),
  },
  { key: "since", header: t("pipeline.sincePersona"), className: "text-right", render: (s) => <span className="mono">{s.memories_since_persona ?? "—"}</span> },
  { key: "l1", header: t("pipeline.lastRun", { layer: "L1" }), render: (s) => <TimeCell value={s.last_l1_at} /> },
  { key: "l2", header: t("pipeline.lastRun", { layer: "L2" }), render: (s) => <TimeCell value={s.last_l2_at} /> },
  { key: "l3", header: t("pipeline.lastRun", { layer: "L3" }), render: (s) => <TimeCell value={s.last_l3_at} /> },
];

function TimeCell({ value }: { value: string | null | undefined }) {
  return (
    <span className="mono whitespace-nowrap text-muted" title={formatTime(value)}>
      {formatRelative(value)}
    </span>
  );
}

/** A name, with its id on hover; the id itself once the name is gone. */
function Named({ name, id }: { name: string | null | undefined; id: string | null | undefined }) {
  if (!id) return <span className="mono text-muted">—</span>;
  return name ? <span title={id}>{name}</span> : <span className="mono text-muted">{id}</span>;
}

/** Rows without a user hold the agent's shared layers (L2/L3). */
function Shared() {
  const t = useT();
  return <span className="text-muted">{t("pipeline.shared")}</span>;
}

/** Into the memory browser, only where Core lets the caller read. */
function OpenLink({ row }: { row: ScopeState }) {
  const t = useT();
  if (!row.can_read_memory || !row.team_id || !row.agent_id) return null;
  const scope = { team_id: row.team_id, agent_id: row.agent_id, user_id: row.user_id ?? "" };
  return (
    <Link to={memoryLink(scope, row.user_id ? "l1" : "l2")} className="text-xxs whitespace-nowrap text-accent hover:underline">
      {t("pipeline.openMemory")}
    </Link>
  );
}
