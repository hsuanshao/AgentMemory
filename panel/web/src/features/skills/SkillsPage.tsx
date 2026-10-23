import { useState } from "react";
import { api, type SkillSearchHit, type SkillSummary } from "../../api";
import { Badge } from "../../components/Badge";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { EmptyState, ErrorBanner } from "../../components/Feedback";
import { PageHeader } from "../../components/PageHeader";
import { Pager } from "../../components/Pager";
import { SearchBox } from "../../components/SearchBox";
import { useAsync } from "../../hooks/useAsync";
import { useStore } from "../../hooks/useStore";
import { truncate } from "../../lib/format";
import { scopeStore } from "../memory/scope";
import { ScopePicker } from "../memory/ScopePicker";
import { SkillDetailPanel } from "./SkillDetail";
import { SkillProposals } from "./SkillProposals";

const PAGE = 50;

type Row = SkillSummary & Partial<Pick<SkillSearchHit, "score" | "snippet">>;

export function SkillsPage() {
  const scope = useStore(scopeStore);
  const [query, setQuery] = useState("");
  const [includeArchived, setIncludeArchived] = useState(false);
  const [offset, setOffset] = useState(0);
  const [selected, setSelected] = useState<string>();

  const list = useAsync<{ rows: Row[]; total?: number }>(async () => {
    if (query) return { rows: (await api.skill.search(scope, query, 50)).items };
    const status = includeArchived ? ["active", "archived"] : undefined;
    const res = await api.skill.list(scope, { status }, { limit: PAGE, offset });
    return { rows: res.items, total: res.total };
  }, [scope, query, includeArchived, offset]);

  const columns: ReadonlyArray<Column<Row>> = [
    {
      key: "name",
      header: "Name",
      render: (s) => (
        <div>
          <div className="font-medium">{s.name}</div>
          <div className="text-xxs text-muted">{truncate(s.snippet ?? s.description, 90)}</div>
        </div>
      ),
    },
    { key: "v", header: "Ver", render: (s) => <span className="mono">v{s.version}</span> },
    { key: "status", header: "Status", render: (s) => <Badge tone={s.status === "active" ? "ok" : "neutral"}>{s.status}</Badge> },
    ...(query ? [{ key: "score", header: "Score", render: (s: Row) => <span className="mono">{s.score?.toFixed(3) ?? "—"}</span> }] : []),
  ];

  return (
    <div className="space-y-4">
      <PageHeader title="Skills" subtitle="Team skills with versions (identity fields from the scope below)" />
      <ScopePicker value={scope} onChange={scopeStore.set} />
      <SkillProposals scope={scope} onDecided={list.reload} />
      <div className="grid gap-4 xl:grid-cols-[minmax(360px,1fr)_2fr]">
        <Card
          title={`Skills${list.data?.total !== undefined ? ` (${list.data.total})` : ""}`}
          bodyClassName="p-0"
          actions={
            <label className="flex items-center gap-1 text-xs text-muted">
              <input type="checkbox" checked={includeArchived} onChange={(e) => (setIncludeArchived(e.target.checked), setOffset(0))} />
              archived
            </label>
          }
        >
          <div className="border-b border-line p-2">
            <SearchBox value={query} onSearch={(q) => (setQuery(q), setOffset(0))} placeholder="Search skills" />
          </div>
          <ErrorBanner error={list.error} onRetry={list.reload} className="m-2" />
          <DataTable
            columns={columns}
            rows={list.data?.rows}
            rowKey={(s) => s.skill_id}
            loading={list.loading}
            empty="No skills"
            onRowClick={(s) => setSelected(s.skill_id)}
            selectedKey={selected}
          />
          {!query && list.data && (
            <Pager offset={offset} limit={PAGE} total={list.data.total} count={list.data.rows.length} onChange={setOffset} />
          )}
        </Card>
        {selected ? (
          <SkillDetailPanel key={selected} scope={scope} skillId={selected} />
        ) : (
          <Card>
            <EmptyState>Select a skill to view its SKILL.md and versions.</EmptyState>
          </Card>
        )}
      </div>
    </div>
  );
}
