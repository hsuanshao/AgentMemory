import { useState } from "react";
import { api, type Scope, type SkillVersion } from "../../api";
import { Badge } from "../../components/Badge";
import { Card } from "../../components/Card";
import { CodeBlock, JsonView, KeyValues } from "../../components/CodeBlock";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner, Spinner } from "../../components/Feedback";
import { useAsync } from "../../hooks/useAsync";
import { formatTime } from "../../lib/format";
import { useT, withCount } from "../../i18n";

export function SkillDetailPanel({ scope, skillId }: { scope: Scope; skillId: string }) {
  const t = useT();
  const [version, setVersion] = useState<number>();
  const detail = useAsync(() => api.skill.get(scope, skillId, version), [scope, skillId, version]);
  const versions = useAsync(() => api.skill.versions(scope, skillId), [scope, skillId]);
  const s = detail.data;

  const versionColumns: ReadonlyArray<Column<SkillVersion>> = [
    { key: "v", header: t("common.version"), render: (v) => <span className="mono">v{v.version}</span> },
    {
      key: "flags",
      header: "",
      render: (v) => (
        <span className="flex gap-1">
          {v.is_head && <Badge tone="accent">{t("skill.head")}</Badge>}
          {v.is_expired && <Badge tone="warn">{t("skill.expired")}</Badge>}
          <Badge tone={v.status === "active" ? "ok" : "neutral"}>{v.status}</Badge>
        </span>
      ),
    },
    { key: "updated", header: t("common.updated"), render: (v) => <span className="mono text-muted">{formatTime(v.updated_at_ms ?? v.created_at_ms)}</span> },
  ];

  return (
    <div className="space-y-4">
      <Card title={s ? s.name : t("skill.fallbackTitle")} actions={s && <Badge tone={s.status === "active" ? "ok" : "neutral"}>{s.status}</Badge>}>
        {detail.loading && <Spinner />}
        <ErrorBanner error={detail.error} onRetry={detail.reload} />
        {s && (
          <div className="space-y-3">
            {s.description && <p className="text-muted">{s.description}</p>}
            <KeyValues
              entries={[
                ["skill_id", s.skill_id],
                [t("common.version"), `v${s.version}${s.is_head ? t("skill.headSuffix") : ""}`],
                [t("common.owner"), `${s.owner_user_id} / ${s.owner_agent_id}`],
                [t("common.team"), s.team_id],
                [t("common.updated"), formatTime(s.updated_at_ms)],
                [t("skill.contentHash"), s.content_hash],
              ]}
            />
            <div>
              <div className="label mb-1">SKILL.md</div>
              <CodeBlock>{s.content ?? t("skill.noContent")}</CodeBlock>
            </div>
            {s.manifest !== undefined && s.manifest !== null && (
              <details>
                <summary className="label cursor-pointer">{t("skill.manifest")}</summary>
                <JsonView value={s.manifest} className="mt-2" />
              </details>
            )}
          </div>
        )}
      </Card>
      <Card title={withCount(t("skill.versions"), versions.data?.total)} bodyClassName="p-0">
        <ErrorBanner error={versions.error} className="m-3" />
        <DataTable
          columns={versionColumns}
          rows={versions.data?.items}
          rowKey={(v) => String(v.version)}
          loading={versions.loading}
          onRowClick={(v) => setVersion(v.is_head ? undefined : v.version)}
          selectedKey={String(version ?? s?.version ?? "")}
        />
      </Card>
    </div>
  );
}
