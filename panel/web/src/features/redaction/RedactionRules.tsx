import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { api, type RedactionLevel, type RedactionRule, type RedactionTestResult } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { CodeBlock } from "../../components/CodeBlock";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner, Notice } from "../../components/Feedback";
import { Field, Input, Select, TextArea, type Option } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { useAsync } from "../../hooks/useAsync";
import { useT, type Translate } from "../../i18n";
import { rich } from "../../i18n/rich";
import { mono, time } from "../meta/shared";

/**
 * The redaction rules of one level (every team, a team, or a user) and the
 * levels above it, which its writes also go through (#57). Core says
 * whether the viewer may change this level (`can_edit`); the levels above
 * are listed read-only. At a user's level, `teams` lets a test add one of
 * their teams' rules, as their writes there go through them.
 */
export function RedactionRules({
  level,
  title,
  teams,
}: {
  level: RedactionLevel;
  title: string;
  teams?: ReadonlyArray<Option<string>>;
}) {
  const t = useT();
  const listing = useAsync(() => api.redaction.list(level), [level.team_id, level.user_id]);
  const [kind, setKind] = useState("");
  const [pattern, setPattern] = useState("");
  const [description, setDescription] = useState("");
  const [sample, setSample] = useState("");
  const [preview, setPreview] = useState<RedactionTestResult>();
  const [testTeam, setTestTeam] = useState(teams?.[0]?.value ?? "");
  // The teams load after this mounts: default to the first until one is picked.
  const picked = useRef(false);
  const firstTeam = teams?.[0]?.value ?? "";
  useEffect(() => {
    if (!picked.current) setTestTeam(firstTeam);
  }, [firstTeam]);

  const create = useAction(async () => {
    await api.redaction.create({ ...level, kind: kind.trim(), pattern, description: description.trim() || undefined });
    setKind("");
    setPattern("");
    setDescription("");
    setPreview(undefined);
    await listing.reload();
  });
  const toggle = useAction(async (rule: RedactionRule) => {
    await api.redaction.setEnabled(rule.rule_id, !rule.enabled);
    await listing.reload();
  });
  // With a pattern typed in: what this level's rules plus it would do.
  const test = useAction(async () => {
    setPreview(
      await api.redaction.test({
        ...level,
        ...(level.user_id && testTeam ? { team_id: testTeam } : {}),
        text: sample,
        ...(pattern ? { pattern, kind: kind.trim() || undefined } : {}),
      }),
    );
  });
  function submit(e: FormEvent) {
    e.preventDefault();
    void create.run();
  }

  const data = listing.data;
  const allTeams = !level.team_id && !level.user_id;
  const own = (level.team_id ? data?.team : level.user_id ? data?.user : data?.department) ?? undefined;
  const editable = !!data?.can_edit;
  const actions: Column<RedactionRule> = {
    key: "actions",
    header: "",
    className: "text-right",
    render: (r) => (
      <Button
        size="sm"
        variant={r.enabled ? "danger" : "secondary"}
        onClick={() => (!r.enabled || window.confirm(t("redaction.confirmDisable", { kind: r.kind }))) && void toggle.run(r)}
      >
        {r.enabled ? t("redaction.disable") : t("redaction.enable")}
      </Button>
    ),
  };

  return (
    <div className="space-y-4">
      <Card title={own ? t("redaction.enabledCount", { label: title, count: own.filter((r) => r.enabled).length }) : title} bodyClassName="p-0">
        <ErrorBanner error={listing.error ?? create.error ?? toggle.error} onRetry={listing.reload} className="m-3" />
        {editable ? (
          <form onSubmit={submit} className="space-y-2 border-b border-line p-3">
            <div className="flex flex-wrap items-end gap-2">
              <Field label={t("common.kind")}>
                <Input value={kind} onChange={(e) => setKind(e.target.value)} placeholder={t("redaction.kindPlaceholder")} className="w-44" />
              </Field>
              <Field label={t("redaction.patternLabel")}>
                <Input value={pattern} onChange={(e) => setPattern(e.target.value)} placeholder={String.raw`\bEMP-\d{6}\b`} className="mono w-80" />
              </Field>
              <Field label={t("common.description")}>
                <Input value={description} onChange={(e) => setDescription(e.target.value)} placeholder={t("common.optional")} className="w-56" />
              </Field>
              <Button type="submit" variant="primary" busy={create.busy} disabled={!kind.trim() || !pattern}>
                {t("redaction.addRule")}
              </Button>
            </div>
            <span className="text-xxs text-muted">{rich(t("redaction.applyNote"), { keep: <span className="mono">keep</span> })}</span>
          </form>
        ) : (
          data && (
            <div className="p-3">
              <Notice>{level.team_id ? t("redaction.readOnlyTeam") : t("redaction.readOnlyAll")}</Notice>
            </div>
          )
        )}
        <DataTable
          columns={editable ? [...ruleColumns(t), actions] : ruleColumns(t)}
          rows={own}
          rowKey={(r) => r.rule_id}
          loading={listing.loading}
          empty={t("redaction.empty")}
        />
      </Card>

      {data && !allTeams && (
        <Card title={t("redaction.alsoApplied", { label: t("redaction.allRules") })} bodyClassName="p-0">
          <DataTable columns={ruleColumns(t)} rows={data.department} rowKey={(r) => r.rule_id} empty={t("redaction.empty")} />
        </Card>
      )}

      <Card title={t("redaction.tryIt")}>
        <div className="space-y-2">
          <ErrorBanner error={test.error} />
          <TextArea value={sample} onChange={(e) => setSample(e.target.value)} rows={3} placeholder={t("redaction.samplePlaceholder")} className="w-full" />
          <div className="flex flex-wrap items-center gap-2">
            {teams && (
              <Select
                aria-label={t("redaction.testInTeam")}
                title={t("redaction.testInTeam")}
                options={[{ value: "", label: t("redaction.testNoTeam") }, ...teams]}
                value={testTeam}
                onChange={(e) => {
                  picked.current = true;
                  setTestTeam(e.target.value);
                }}
              />
            )}
            <Button busy={test.busy} disabled={!sample} onClick={() => void test.run()}>
              {t("redaction.test")}
            </Button>
            <span className="text-xxs text-muted">{pattern ? t("redaction.withPattern") : t("redaction.withRules")}</span>
          </div>
          {preview && <Preview preview={preview} />}
        </div>
      </Card>

      {data && (
        <Card title={t("redaction.builtin")}>
          <div className="space-y-2 text-xs">
            <div className="flex flex-wrap gap-1">
              {data.builtin
                .filter((k) => k !== "password-shape" || data.deployment_password_shape)
                .map((k) => (
                  <Badge key={k}>{k}</Badge>
                ))}
            </div>
            {data.deployment.length > 0 ? (
              <ul className="space-y-1">
                {data.deployment.map((p) => (
                  <li key={p.kind}>
                    <Badge>{p.kind}</Badge> <span className="mono break-all">{p.pattern}</span>
                  </li>
                ))}
              </ul>
            ) : (
              <span className="text-muted">{t("redaction.noDeployment")}</span>
            )}
          </div>
        </Card>
      )}
    </div>
  );
}

const ruleColumns = (t: Translate): Column<RedactionRule>[] => [
  { key: "kind", header: t("common.kind"), render: (r) => <Badge tone="accent">{r.kind}</Badge> },
  { key: "pattern", header: t("redaction.pattern"), render: (r) => <span className="mono break-all">{r.pattern}</span> },
  { key: "desc", header: t("common.description"), render: (r) => r.description || "—" },
  {
    key: "status",
    header: t("common.status"),
    render: (r) => <Badge tone={r.enabled ? "ok" : "neutral"}>{r.enabled ? t("redaction.enabled") : t("redaction.disabled")}</Badge>,
  },
  { key: "updated", header: t("redaction.changed"), render: (r) => <>{time(r.updated_at)} {mono(r.updated_by)}</> },
];

function Preview({ preview }: { preview: RedactionTestResult }) {
  const t = useT();
  const share = preview.pattern_share;
  let warning: ReactNode = null;
  if (preview.refused) {
    warning = (
      <div role="alert" className="rounded-sm border border-err/40 bg-err/10 px-3 py-2 text-xs text-err">
        {t("redaction.refused", { reason: preview.refused })}
      </div>
    );
  } else if (share != null && share > 0.2) {
    warning = (
      <div role="alert" className="rounded-sm border border-warn/40 bg-warn/10 px-3 py-2 text-xs">
        {t("redaction.shareWarning", { percent: Math.round(share * 100) })}
      </div>
    );
  }
  return (
    <>
      {warning}
      {share != null && <span className="text-xxs text-muted">{t("redaction.share", { percent: Math.round(share * 100) })}</span>}
      <CodeBlock>{preview.redacted}</CodeBlock>
      <div className="flex flex-wrap gap-1">
        {preview.hits.length === 0 ? (
          <span className="text-xxs text-muted">{t("redaction.nothing")}</span>
        ) : (
          preview.hits.map((h) => (
            <Badge key={h.kind} tone="accent">
              {h.kind} × {h.count}
            </Badge>
          ))
        )}
      </div>
    </>
  );
}
