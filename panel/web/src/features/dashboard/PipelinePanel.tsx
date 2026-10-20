import { Link } from "react-router-dom";
import type { PipelineStatus, ScopeState } from "../../api";
import { Badge } from "../../components/Badge";
import { Card, Stat } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { formatNumber, formatRelative, formatTime } from "../../lib/format";
import { memoryLink } from "../memory/scope";
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
        <DataTable columns={SCOPE_COLUMNS} rows={p?.scopes} rowKey={(s) => s.key} loading={!p && !slot.error} empty="No scopes yet" />
      </Card>
      <ErrorBanner error={slot.error} />
    </>
  );
}

const SCOPE_COLUMNS: ReadonlyArray<Column<ScopeState>> = [
  { key: "team", header: "Team", render: (s) => <Named name={s.team_name} id={s.team_id} /> },
  { key: "agent", header: "Agent", render: (s) => <Named name={s.agent_name} id={s.agent_id} /> },
  { key: "user", header: "User", render: (s) => (s.user_id ? <Named name={s.username} id={s.user_id} /> : <Shared />) },
  {
    key: "session",
    header: "Session",
    render: (s) => <span className="mono text-muted">{s.session_id ?? "—"}</span>,
  },
  { key: "open", header: "", render: (s) => <OpenLink row={s} /> },
  {
    key: "conv",
    header: "Conversations",
    className: "text-right",
    render: (s) => (
      <span className="mono">
        {s.conversation_count ?? "—"}
        {s.warmup_threshold != null && <span className="text-muted"> / {s.warmup_threshold}</span>}
      </span>
    ),
  },
  { key: "cursor", header: "L1 cursor", render: (s) => <span className="mono">{s.l1_cursor ?? "—"}</span> },
  {
    key: "l2p",
    header: "L2 pending",
    className: "text-right",
    render: (s) => (s.l2_pending ? <Badge tone="warn">pending</Badge> : <span className="mono text-muted">—</span>),
  },
  { key: "since", header: "Since persona", className: "text-right", render: (s) => <span className="mono">{s.memories_since_persona ?? "—"}</span> },
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

/** A name, with its id on hover; the id itself once the name is gone. */
function Named({ name, id }: { name: string | null | undefined; id: string | null | undefined }) {
  if (!id) return <span className="mono text-muted">—</span>;
  return name ? <span title={id}>{name}</span> : <span className="mono text-muted">{id}</span>;
}

/** Rows without a user hold the agent's shared layers (L2/L3). */
function Shared() {
  return <span className="text-muted">shared</span>;
}

/** Into the memory browser, only where Core lets the caller read. */
function OpenLink({ row }: { row: ScopeState }) {
  if (!row.can_read_memory || !row.team_id || !row.agent_id) return null;
  const scope = { team_id: row.team_id, agent_id: row.agent_id, user_id: row.user_id ?? "" };
  return (
    <Link to={memoryLink(scope, row.user_id ? "l1" : "l2")} className="text-xxs whitespace-nowrap text-accent hover:underline">
      Open memory
    </Link>
  );
}
