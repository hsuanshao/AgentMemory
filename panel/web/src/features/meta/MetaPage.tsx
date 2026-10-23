import { useState } from "react";
import { api, type Asset } from "../../api";
import { Badge } from "../../components/Badge";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Select } from "../../components/Field";
import { PageHeader } from "../../components/PageHeader";
import { Tabs, type TabItem } from "../../components/Tabs";
import { useAsync } from "../../hooks/useAsync";
import { useStore } from "../../hooks/useStore";
import { isSystemAdmin, sessionStore } from "../auth/session";
import { LIST_LIMIT, mono, time } from "./shared";
import { RedactionPanel } from "./RedactionPanel";
import { TeamsPanel } from "./TeamsPanel";
import { UsersPanel } from "./UsersPanel";

type TabId = "teams" | "users" | "assets" | "redaction";

const ASSET_TYPES = [
  { value: "", label: "All types" },
  { value: "skill", label: "skill" },
  { value: "llm_wiki", label: "llm_wiki" },
  { value: "code_graph", label: "code_graph" },
  { value: "chat_memory", label: "chat_memory" },
] as const;

const ASSET_COLUMNS: ReadonlyArray<Column<Asset>> = [
  { key: "name", header: "Asset", render: (a) => <span className="font-medium">{a.name}</span> },
  { key: "type", header: "Type", render: (a) => <Badge tone="accent">{a.asset_type}</Badge> },
  { key: "id", header: "asset_id", render: (a) => mono(a.asset_id) },
  { key: "team", header: "Team", render: (a) => mono(a.team_id) },
  { key: "vis", header: "Visibility", render: (a) => <Badge>{a.visibility}</Badge> },
  { key: "ver", header: "Ver", render: (a) => mono(`v${a.version}`) },
  { key: "usage", header: "Uses", className: "text-right", render: (a) => mono(String(a.usage_count)) },
  { key: "updated", header: "Updated", render: (a) => time(a.updated_at) },
];

export function MetaPage() {
  const session = useStore(sessionStore);
  const userId = session?.user.user_id ?? "";
  const admin = isSystemAdmin(session);
  const [tab, setTab] = useState<TabId>("teams");
  const tabs: TabItem<TabId>[] = [
    { id: "teams", label: "Teams" },
    ...(admin ? [{ id: "users" as const, label: "Users" }] : []),
    { id: "assets", label: "Assets" },
    ...(admin ? [{ id: "redaction" as const, label: "Redaction" }] : []),
  ];

  return (
    <div className="space-y-4">
      <PageHeader
        title="Metadata"
        subtitle={
          admin ? (
            "Manage users, teams, membership and agents"
          ) : (
            <>
              Read-only view of the teams <span className="mono">{userId}</span> belongs to
            </>
          )
        }
      />
      <Tabs items={tabs} value={tab} onChange={setTab} />
      {tab === "teams" && <TeamsPanel userId={userId} admin={admin} />}
      {tab === "users" && admin && <UsersPanel />}
      {tab === "assets" && <AssetsPanel userId={userId} />}
      {tab === "redaction" && admin && <RedactionPanel />}
    </div>
  );
}

function AssetsPanel({ userId }: { userId: string }) {
  const [assetType, setAssetType] = useState("");
  const assets = useAsync(
    () => api.meta.accessibleAssets(userId, { asset_type: assetType || undefined, limit: LIST_LIMIT }),
    [userId, assetType],
    !!userId,
  );
  return (
    <Card
      title={`Accessible assets${assets.data ? ` (${assets.data.total})` : ""}`}
      bodyClassName="p-0"
      actions={<Select aria-label="Asset type" options={ASSET_TYPES} value={assetType} onChange={(e) => setAssetType(e.target.value)} />}
    >
      <ErrorBanner error={assets.error} onRetry={assets.reload} className="m-3" />
      <DataTable columns={ASSET_COLUMNS} rows={assets.data?.items} rowKey={(a) => a.asset_id} loading={assets.loading} empty="No accessible assets" />
    </Card>
  );
}
