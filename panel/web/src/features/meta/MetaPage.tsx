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
import { AuditPanel } from "./AuditPanel";
import { RedactionPanel } from "./RedactionPanel";
import { TeamsPanel } from "./TeamsPanel";
import { UsersPanel } from "./UsersPanel";
import { useT, withCount, type Translate } from "../../i18n";
import { rich } from "../../i18n/rich";

type TabId = "teams" | "users" | "assets" | "redaction" | "audit";

const assetTypes = (t: Translate) => [
  { value: "", label: t("common.allTypes") },
  { value: "skill", label: "skill" },
  { value: "llm_wiki", label: "llm_wiki" },
  { value: "code_graph", label: "code_graph" },
  { value: "chat_memory", label: "chat_memory" },
];

const assetColumns = (t: Translate): ReadonlyArray<Column<Asset>> => [
  { key: "name", header: t("assets.asset"), render: (a) => <span className="font-medium">{a.name}</span> },
  { key: "type", header: t("common.type"), render: (a) => <Badge tone="accent">{a.asset_type}</Badge> },
  { key: "id", header: "asset_id", render: (a) => mono(a.asset_id) },
  { key: "team", header: t("common.team"), render: (a) => mono(a.team_id) },
  { key: "vis", header: t("assets.visibility"), render: (a) => <Badge>{a.visibility}</Badge> },
  { key: "ver", header: t("common.versionShort"), render: (a) => mono(`v${a.version}`) },
  { key: "usage", header: t("assets.uses"), className: "text-right", render: (a) => mono(String(a.usage_count)) },
  { key: "updated", header: t("common.updated"), render: (a) => time(a.updated_at) },
];

export function MetaPage() {
  const t = useT();
  const session = useStore(sessionStore);
  const userId = session?.user.user_id ?? "";
  const admin = isSystemAdmin(session);
  const [tab, setTab] = useState<TabId>("teams");
  const tabs: TabItem<TabId>[] = [
    { id: "teams", label: t("meta.tabTeams") },
    ...(admin ? [{ id: "users" as const, label: t("meta.tabUsers") }] : []),
    { id: "assets", label: t("meta.tabAssets") },
    ...(admin ? [{ id: "redaction" as const, label: t("meta.tabRedaction") }] : []),
    ...(admin ? [{ id: "audit" as const, label: t("meta.tabAudit") }] : []),
  ];

  return (
    <div className="space-y-4">
      <PageHeader
        title={t("meta.title")}
        subtitle={admin ? t("meta.subtitleAdmin") : rich(t("meta.subtitleUser"), { user: <span className="mono">{userId}</span> })}
      />
      <Tabs items={tabs} value={tab} onChange={setTab} />
      {tab === "teams" && <TeamsPanel userId={userId} admin={admin} />}
      {tab === "users" && admin && <UsersPanel />}
      {tab === "assets" && <AssetsPanel userId={userId} />}
      {tab === "redaction" && admin && <RedactionPanel />}
      {tab === "audit" && admin && <AuditPanel />}
    </div>
  );
}

function AssetsPanel({ userId }: { userId: string }) {
  const t = useT();
  const [assetType, setAssetType] = useState("");
  const assets = useAsync(
    () => api.meta.accessibleAssets(userId, { asset_type: assetType || undefined, limit: LIST_LIMIT }),
    [userId, assetType],
    !!userId,
  );
  return (
    <Card
      title={withCount(t("assets.title"), assets.data?.total)}
      bodyClassName="p-0"
      actions={<Select aria-label={t("assets.type")} options={assetTypes(t)} value={assetType} onChange={(e) => setAssetType(e.target.value)} />}
    >
      <ErrorBanner error={assets.error} onRetry={assets.reload} className="m-3" />
      <DataTable columns={assetColumns(t)} rows={assets.data?.items} rowKey={(a) => a.asset_id} loading={assets.loading} empty={t("assets.empty")} />
    </Card>
  );
}
