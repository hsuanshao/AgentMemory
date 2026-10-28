import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { api, type GenerationLayer, type GenerationLogItem, type GenerationLogList, type GenerationStatus } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Field, Select } from "../../components/Field";
import { PageHeader } from "../../components/PageHeader";
import { useAction } from "../../hooks/useAction";
import { useAsync } from "../../hooks/useAsync";
import { statusTone } from "../../lib/distillation";
import { formatRelative, formatTime } from "../../lib/format";
import { LogDetail } from "./LogDetail";
import { useT, type Translate } from "../../i18n";

const PAGE = 50;

const layers = (t: Translate) => [
  { value: "", label: t("logs.allLayers") },
  { value: "l1", label: "L1" },
  { value: "l2", label: "L2" },
  { value: "l3", label: "L3" },
];

const statuses = (t: Translate) => [
  { value: "", label: t("logs.allStatuses") },
  { value: "succeeded", label: "succeeded" },
  { value: "empty", label: t("logs.statusEmpty") },
  { value: "failed", label: "failed" },
  { value: "dropped", label: t("logs.statusDropped") },
];

const columns = (t: Translate): ReadonlyArray<Column<GenerationLogItem>> => [
  { key: "time", header: t("logs.finished"), render: (l) => <span className="mono whitespace-nowrap" title={formatRelative(l.finished_at_ms)}>{formatTime(l.finished_at_ms)}</span> },
  { key: "layer", header: t("common.layer"), render: (l) => <Badge>{l.layer}</Badge> },
  { key: "status", header: t("common.status"), render: (l) => <Badge tone={statusTone(l.status)}>{l.status}</Badge> },
  { key: "model", header: t("logs.model"), render: (l) => <span className="mono text-muted">{l.model ?? "—"}</span> },
  {
    key: "latency",
    header: t("logs.latency"),
    className: "text-right",
    render: (l) => <span className="mono text-muted">{l.latency_ms !== undefined ? `${(l.latency_ms / 1000).toFixed(1)} s` : "—"}</span>,
  },
  { key: "session", header: t("common.session"), render: (l) => <span className="mono text-muted">{l.session_id ?? "—"}</span> },
  {
    key: "error",
    header: t("logs.error"),
    render: (l) =>
      l.error_message ? (
        <span className="mono block max-w-md truncate text-err" title={l.error_message}>
          {l.error_message}
        </span>
      ) : null,
  },
];

export function LogsPage() {
  const t = useT();
  const [layer, setLayer] = useState<GenerationLayer | "">("");
  const [params] = useSearchParams();
  const [status, setStatus] = useState<GenerationStatus | "">(() => {
    const s = params.get("status");
    return s === "succeeded" || s === "empty" || s === "failed" || s === "dropped" ? s : "";
  });
  const [selected, setSelected] = useState<string>();
  const [more, setMore] = useState<GenerationLogList>();

  const filter = { layer: layer || undefined, status: status || undefined, limit: PAGE };
  const first = useAsync(() => api.generationLog.list(filter), [layer, status]);
  useEffect(() => setMore(undefined), [first.data]);

  const items = first.data ? [...first.data.items, ...(more?.items ?? [])] : undefined;
  const cursor = more ? more.next_cursor : first.data?.next_cursor;

  const loadMore = useAction(async () => {
    const page = await api.generationLog.list({ ...filter, cursor: cursor ?? undefined });
    setMore((prev) => ({ items: [...(prev?.items ?? []), ...page.items], next_cursor: page.next_cursor }));
  });

  return (
    <div className="space-y-4">
      <PageHeader
        title={t("logs.title")}
        subtitle={t("logs.subtitle")}
        actions={
          <>
            <Field label={t("common.layer")}>
              <Select options={layers(t)} value={layer} onChange={(e) => setLayer(e.target.value as GenerationLayer | "")} />
            </Field>
            <Field label={t("common.status")}>
              <Select options={statuses(t)} value={status} onChange={(e) => setStatus(e.target.value as GenerationStatus | "")} />
            </Field>
            <Button className="self-end" onClick={() => void first.reload()} busy={first.loading}>
              {t("common.refresh")}
            </Button>
          </>
        }
      />
      <ErrorBanner error={first.error ?? loadMore.error} onRetry={() => void first.reload()} />
      <Card bodyClassName="p-0">
        <DataTable
          columns={columns(t)}
          rows={items}
          rowKey={(l) => l.log_id}
          loading={first.loading}
          empty={t("logs.empty")}
          onRowClick={(l) => setSelected(l.log_id)}
          selectedKey={selected}
        />
        {cursor && (
          <div className="flex justify-center border-t border-line p-2">
            <Button size="sm" busy={loadMore.busy} onClick={() => void loadMore.run()}>
              {t("logs.loadMore")}
            </Button>
          </div>
        )}
      </Card>
      <LogDetail logId={selected} onClose={() => setSelected(undefined)} />
    </div>
  );
}
