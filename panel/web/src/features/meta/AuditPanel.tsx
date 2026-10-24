import { useState } from "react";
import { api, type AuditEvent, type AuditKind } from "../../api";
import { Badge, type BadgeTone } from "../../components/Badge";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Select } from "../../components/Field";
import { useAsync } from "../../hooks/useAsync";
import { LIST_LIMIT, mono, time } from "./shared";

const KINDS = [
  { value: "", label: "All events" },
  { value: "credential_rejected", label: "Refused credentials" },
  { value: "source_locked", label: "Locked-out addresses" },
  { value: "key_issued", label: "Keys issued by the operator" },
] as const;

const TONE: Record<AuditKind, BadgeTone> = {
  credential_rejected: "warn",
  source_locked: "err",
  key_issued: "accent",
};

const COLUMNS: ReadonlyArray<Column<AuditEvent>> = [
  { key: "at", header: "Time", render: (e) => time(new Date(e.at_ms).toISOString()) },
  { key: "kind", header: "Event", render: (e) => <Badge tone={TONE[e.kind]}>{e.kind}</Badge> },
  {
    key: "source",
    header: "Address",
    render: (e) => (
      <span title={e.forwarded_for ? `X-Forwarded-For: ${e.forwarded_for}` : undefined}>{mono(e.source)}</span>
    ),
  },
  { key: "path", header: "Path", render: (e) => mono(e.path) },
  { key: "credential", header: "Credential", render: (e) => mono(e.credential) },
  { key: "agent", header: "Client", render: (e) => <span className="break-all text-muted">{e.user_agent ?? "—"}</span> },
  { key: "detail", header: "Detail", render: (e) => e.detail ?? "—" },
];

/**
 * System admins only: refused keys, locked-out addresses and keys the
 * operator issued for this department. A key is never shown, only the
 * start of its digest.
 */
export function AuditPanel() {
  const [kind, setKind] = useState<AuditKind | "">("");
  const events = useAsync(() => api.admin.auditEvents({ kind: kind || undefined, limit: LIST_LIMIT }), [kind]);
  return (
    <Card
      title="Credential audit log"
      bodyClassName="p-0"
      actions={
        <Select aria-label="Event" options={KINDS} value={kind} onChange={(e) => setKind(e.target.value as AuditKind | "")} />
      }
    >
      <p className="px-3 pt-3 text-xs text-muted">
        By default, from the 5th refused credential in 15 minutes an address waits longer for each answer, and the 20th locks
        it out for 15 minutes. Events are kept 90 days.
      </p>
      <ErrorBanner error={events.error} onRetry={events.reload} className="m-3" />
      <DataTable columns={COLUMNS} rows={events.data?.items} rowKey={(e) => e.event_id} loading={events.loading} empty="No events" />
    </Card>
  );
}
