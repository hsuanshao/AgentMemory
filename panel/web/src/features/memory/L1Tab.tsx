import { useState } from "react";
import { api, ATOMIC_TYPES, type AtomicDetail, type AtomicType, type Scope } from "../../api";
import { Badge, type BadgeTone } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Select } from "../../components/Field";
import { Pager } from "../../components/Pager";
import { SearchBox } from "../../components/SearchBox";
import { useAction } from "../../hooks/useAction";
import { useIsAdmin } from "../auth/session";
import { useAsync } from "../../hooks/useAsync";
import { formatTime } from "../../lib/format";
import { AtomEditor } from "./AtomEditor";
import { useT } from "../../i18n";

const PAGE = 20;
const TYPE_TONE: Record<AtomicType, BadgeTone> = { episodic: "accent", persona: "ok", instruction: "warn" };

interface L1View {
  rows: AtomicDetail[];
  total?: number;
}

export function L1Tab({ scope }: { scope: Scope }) {
  const t = useT();
  const [type, setType] = useState<AtomicType | "">("");
  const [query, setQuery] = useState("");
  const [offset, setOffset] = useState(0);
  const [editing, setEditing] = useState<AtomicDetail>();

  const list = useAsync<L1View>(async () => {
    const only = type || undefined;
    if (query) return { rows: (await api.atomic.search(scope, query, only, 50)).items };
    const res = await api.atomic.query(scope, { type: only, limit: PAGE, offset });
    return { rows: res.items, total: res.total };
  }, [scope, type, query, offset]);

  const remove = useAction(async (id: string) => {
    await api.atomic.delete(scope, [id]);
    await list.reload();
  });

  const admin = useIsAdmin();
  const columns: ReadonlyArray<Column<AtomicDetail>> = [
    { key: "type", header: t("common.type"), render: (a) => <Badge tone={TYPE_TONE[a.type] ?? "neutral"}>{a.type}</Badge> },
    {
      key: "content",
      header: t("common.content"),
      render: (a) => (
        <div className="max-w-3xl">
          <div className="whitespace-pre-wrap wrap-break-word">{a.content}</div>
          {a.background && <div className="mt-1 text-xs text-muted">↳ {a.background}</div>}
        </div>
      ),
    },
    { key: "version", header: t("common.versionShort"), render: (a) => <span className="mono">v{String(a.version).replace(/^v/, "")}</span> },
    ...(query ? [{ key: "score", header: t("common.score"), render: (a: AtomicDetail) => <span className="mono">{a.score?.toFixed(3) ?? "—"}</span> }] : []),
    { key: "updated", header: t("common.updated"), render: (a) => <span className="mono whitespace-nowrap text-muted">{formatTime(a.updated_at)}</span> },
    ...(admin ? [{
      key: "actions",
      header: "",
      className: "text-right whitespace-nowrap",
      render: (a: AtomicDetail) => (
        <div className="flex justify-end gap-1">
          <Button size="sm" onClick={() => setEditing(a)}>
            {t("common.edit")}
          </Button>
          <Button size="sm" variant="danger" onClick={() => window.confirm(t("l1.confirmDelete", { id: a.id })) && void remove.run(a.id)}>
            {t("common.delete")}
          </Button>
        </div>
      ),
    }] : []),
  ];

  return (
    <Card
      title={t("l1.title")}
      bodyClassName="p-0"
      actions={
        <>
          <Select
            options={[{ value: "", label: t("common.allTypes") }, ...ATOMIC_TYPES.map((k) => ({ value: k, label: k }))]}
            value={type}
            onChange={(e) => {
              setType(e.target.value as AtomicType | "");
              setOffset(0);
            }}
          />
          <SearchBox value={query} onSearch={(q) => (setQuery(q), setOffset(0))} placeholder={t("memory.keywordSearch")} />
        </>
      }
    >
      <ErrorBanner error={list.error ?? remove.error} onRetry={list.reload} className="m-3" />
      <DataTable columns={columns} rows={list.data?.rows} rowKey={(a) => a.id} loading={list.loading} empty={t("l1.empty")} />
      {!query && list.data && (
        <Pager offset={offset} limit={PAGE} total={list.data.total} count={list.data.rows.length} onChange={setOffset} />
      )}
      <AtomEditor
        scope={scope}
        atom={editing}
        onClose={() => setEditing(undefined)}
        onSaved={() => {
          setEditing(undefined);
          void list.reload();
        }}
      />
    </Card>
  );
}
