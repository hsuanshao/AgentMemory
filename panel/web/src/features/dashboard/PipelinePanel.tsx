import type { PipelineStatus, ScopeState } from "../../api";
import { Badge } from "../../components/Badge";
import { Card, Stat } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { formatNumber, formatRelative, formatTime } from "../../lib/format";
import type { Slot } from "./useDashboardData";

export function PipelinePanel({ slot }: { slot: Slot<PipelineStatus> }) {
  const p = slot.data;
  return (
    <>
      <div className="grid gap-4 xl:grid-cols-[2fr_1fr]">
        <Card title="Pipeline worker">
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
            <Stat label="Enqueued" value={formatNumber(p?.worker.tasks_enqueued)} />
            <Stat label="Completed" value={formatNumber(p?.worker.tasks_completed)} tone="ok" />
            <Stat label="Failed" value={formatNumber(p?.worker.tasks_failed)} tone={p?.worker.tasks_failed ? "err" : "default"} />
            <Stat label="Retried" value={formatNumber(p?.worker.tasks_retried)} tone={p?.worker.tasks_retried ? "warn" : "default"} />
            <Stat
              label="Dead-lettered"
              value={formatNumber(p?.worker.tasks_dead_lettered)}
              tone={p?.worker.tasks_dead_lettered ? "err" : "default"}
            />
            <Stat label="Last task" value={formatRelative(p?.worker.last_task_at)} hint={formatTime(p?.worker.last_task_at)} />
          </div>
          {p?.worker.last_error && (
            <div className="mono mt-3 rounded-sm border border-err/30 bg-err/5 px-2 py-1.5 text-err">last error: {p.worker.last_error}</div>
          )}
        </Card>
        <Card title="Queue">
          <div className="grid grid-cols-3 gap-2">
            <Stat label="Pending" value={formatNumber(p?.queue.pending)} tone={p?.queue.pending ? "warn" : "default"} />
            <Stat label="Running" value={formatNumber(p?.queue.running)} tone={p?.queue.running ? "accent" : "default"} />
            <Stat label="Dead letter" value={formatNumber(p?.queue.dead_letter)} tone={p?.queue.dead_letter ? "err" : "default"} />
          </div>
        </Card>
      </div>
      <Card title={`Scope states${p ? ` (${p.scopes.length})` : ""}`} bodyClassName="p-0">
        <DataTable columns={SCOPE_COLUMNS} rows={p?.scopes} rowKey={(s) => s.scope} loading={!p && !slot.error} empty="No scopes yet" />
      </Card>
      <ErrorBanner error={slot.error} />
    </>
  );
}

const SCOPE_COLUMNS: ReadonlyArray<Column<ScopeState>> = [
  { key: "scope", header: "Scope", render: (s) => <span className="mono">{s.scope}</span> },
  {
    key: "ids",
    header: "team / agent / user / session",
    render: (s) => (
      <span className="mono text-muted">{[s.team_id, s.agent_id, s.user_id, s.session_id].map((v) => v ?? "·").join(" / ")}</span>
    ),
  },
  {
    key: "conv",
    header: "Conversations",
    className: "text-right",
    render: (s) => (
      <span className="mono">
        {s.conversation_count}
        <span className="text-muted"> / {s.warmup_threshold}</span>
      </span>
    ),
  },
  { key: "cursor", header: "L1 cursor", render: (s) => <span className="mono">{s.l1_cursor ?? "—"}</span> },
  {
    key: "l2p",
    header: "L2 pending",
    className: "text-right",
    render: (s) => (s.l2_pending > 0 ? <Badge tone="warn">{s.l2_pending}</Badge> : <span className="mono text-muted">0</span>),
  },
  { key: "since", header: "Since persona", className: "text-right", render: (s) => <span className="mono">{s.memories_since_persona}</span> },
  { key: "l1", header: "Last L1", render: (s) => <TimeCell value={s.last_l1_at} /> },
  { key: "l2", header: "Last L2", render: (s) => <TimeCell value={s.last_l2_at} /> },
  { key: "l3", header: "Last L3", render: (s) => <TimeCell value={s.last_l3_at} /> },
];

function TimeCell({ value }: { value: string | null | undefined }) {
  return (
    <span className="mono whitespace-nowrap text-muted" title={formatTime(value)}>
      {formatRelative(value)}
    </span>
  );
}
