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

const PAGE = 20;
const TYPE_TONE: Record<AtomicType, BadgeTone> = { episodic: "accent", persona: "ok", instruction: "warn" };
const TYPE_OPTIONS = [{ value: "", label: "All types" }, ...ATOMIC_TYPES.map((t) => ({ value: t, label: t }))];

interface L1View {
  rows: AtomicDetail[];
  total?: number;
}

export function L1Tab({ scope }: { scope: Scope }) {
  const [type, setType] = useState<AtomicType | "">("");
  const [query, setQuery] = useState("");
  const [offset, setOffset] = useState(0);
  const [editing, setEditing] = useState<AtomicDetail>();

  const list = useAsync<L1View>(async () => {
    const t = type || undefined;
    if (query) return { rows: (await api.atomic.search(scope, query, t, 50)).items };
    const res = await api.atomic.query(scope, { type: t, limit: PAGE, offset });
    return { rows: res.items, total: res.total };
  }, [scope, type, query, offset]);

  const remove = useAction(async (id: string) => {
    await api.atomic.delete(scope, [id]);
    await list.reload();
  });

  const admin = useIsAdmin();
  const columns: ReadonlyArray<Column<AtomicDetail>> = [
    { key: "type", header: "Type", render: (a) => <Badge tone={TYPE_TONE[a.type] ?? "neutral"}>{a.type}</Badge> },
    {
      key: "content",
      header: "Content",
      render: (a) => (
        <div className="max-w-3xl">
          <div className="whitespace-pre-wrap wrap-break-word">{a.content}</div>
          {a.background && <div className="mt-1 text-xs text-muted">↳ {a.background}</div>}
        </div>
      ),
    },
    { key: "version", header: "Ver", render: (a) => <span className="mono">v{String(a.version).replace(/^v/, "")}</span> },
    ...(query ? [{ key: "score", header: "Score", render: (a: AtomicDetail) => <span className="mono">{a.score?.toFixed(3) ?? "—"}</span> }] : []),
    { key: "updated", header: "Updated", render: (a) => <span className="mono whitespace-nowrap text-muted">{formatTime(a.updated_at)}</span> },
    ...(admin ? [{
      key: "actions",
      header: "",
      className: "text-right whitespace-nowrap",
      render: (a: AtomicDetail) => (
        <div className="flex justify-end gap-1">
          <Button size="sm" onClick={() => setEditing(a)}>
            Edit
          </Button>
          <Button size="sm" variant="danger" onClick={() => window.confirm(`Delete atom ${a.id}?`) && void remove.run(a.id)}>
            Delete
          </Button>
        </div>
      ),
    }] : []),
  ];

  return (
    <Card
      title="L1 · Atomic memories"
      bodyClassName="p-0"
      actions={
        <>
          <Select
            options={TYPE_OPTIONS}
            value={type}
            onChange={(e) => {
              setType(e.target.value as AtomicType | "");
              setOffset(0);
            }}
          />
          <SearchBox value={query} onSearch={(q) => (setQuery(q), setOffset(0))} placeholder="Keyword search" />
        </>
      }
    >
      <ErrorBanner error={list.error ?? remove.error} onRetry={list.reload} className="m-3" />
      <DataTable columns={columns} rows={list.data?.rows} rowKey={(a) => a.id} loading={list.loading} empty="No atoms in this scope" />
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
