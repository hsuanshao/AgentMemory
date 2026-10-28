import { useState } from "react";
import { api, type AuditEvent, type AuditKind } from "../../api";
import { Badge, type BadgeTone } from "../../components/Badge";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Select } from "../../components/Field";
import { useAsync } from "../../hooks/useAsync";
import { LIST_LIMIT, mono, time } from "./shared";
import { useT, type Translate } from "../../i18n";

const kinds = (t: Translate) => [
  { value: "", label: t("audit.allEvents") },
  { value: "credential_rejected", label: t("audit.credentialRejected") },
  { value: "source_locked", label: t("audit.sourceLocked") },
  { value: "key_issued", label: t("audit.keyIssued") },
];

const TONE: Record<AuditKind, BadgeTone> = {
  credential_rejected: "warn",
  source_locked: "err",
  key_issued: "accent",
};

const columns = (t: Translate): ReadonlyArray<Column<AuditEvent>> => [
  { key: "at", header: t("common.time"), render: (e) => time(new Date(e.at_ms).toISOString()) },
  { key: "kind", header: t("audit.event"), render: (e) => <Badge tone={TONE[e.kind]}>{e.kind}</Badge> },
  {
    key: "source",
    header: t("audit.address"),
    render: (e) => (
      <span title={e.forwarded_for ? `X-Forwarded-For: ${e.forwarded_for}` : undefined}>{mono(e.source)}</span>
    ),
  },
  { key: "path", header: t("audit.path"), render: (e) => mono(e.path) },
  { key: "credential", header: t("audit.credential"), render: (e) => mono(e.credential) },
  { key: "agent", header: t("audit.client"), render: (e) => <span className="break-all text-muted">{e.user_agent ?? "—"}</span> },
  { key: "detail", header: t("audit.detail"), render: (e) => e.detail ?? "—" },
];

/**
 * System admins only: refused keys, locked-out addresses and keys the
 * operator issued for this department. A key is never shown, only the
 * start of its digest.
 */
export function AuditPanel() {
  const t = useT();
  const [kind, setKind] = useState<AuditKind | "">("");
  const events = useAsync(() => api.admin.auditEvents({ kind: kind || undefined, limit: LIST_LIMIT }), [kind]);
  return (
    <Card
      title={t("audit.title")}
      bodyClassName="p-0"
      actions={
        <Select aria-label={t("audit.event")} options={kinds(t)} value={kind} onChange={(e) => setKind(e.target.value as AuditKind | "")} />
      }
    >
      <p className="px-3 pt-3 text-xs text-muted">
        {t("audit.note")}
      </p>
      <ErrorBanner error={events.error} onRetry={events.reload} className="m-3" />
      <DataTable columns={columns(t)} rows={events.data?.items} rowKey={(e) => e.event_id} loading={events.loading} empty={t("audit.empty")} />
    </Card>
  );
}
