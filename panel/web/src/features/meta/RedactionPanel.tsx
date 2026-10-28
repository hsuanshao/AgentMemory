import { useState, type FormEvent } from "react";
import { api, type RedactionRule, type RedactionTestResult } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { CodeBlock } from "../../components/CodeBlock";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Field, Input, TextArea } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { useAsync } from "../../hooks/useAsync";
import { mono, time } from "./shared";
import { useT } from "../../i18n";
import { rich } from "../../i18n/rich";

/**
 * System admins only: the redaction rules the department's writes go through.
 * The built-in and deployment rules are listed for reference; only the
 * department's own can be added, changed or disabled here (#57).
 */
export function RedactionPanel() {
  const t = useT();
  const listing = useAsync(() => api.admin.redactionRules(), []);
  const [kind, setKind] = useState("");
  const [pattern, setPattern] = useState("");
  const [description, setDescription] = useState("");
  const [sample, setSample] = useState("");
  const [preview, setPreview] = useState<RedactionTestResult>();

  const create = useAction(async () => {
    await api.admin.createRedactionRule({ kind: kind.trim(), pattern, description: description.trim() || undefined });
    setKind("");
    setPattern("");
    setDescription("");
    setPreview(undefined);
    await listing.reload();
  });
  const toggle = useAction(async (rule: RedactionRule) => {
    await api.admin.setRedactionRuleEnabled(rule.rule_id, !rule.enabled);
    await listing.reload();
  });
  // With a pattern typed in: what the department's rules plus it would do.
  const test = useAction(async () => {
    setPreview(
      await api.admin.testRedaction({
        text: sample,
        ...(pattern ? { pattern, kind: kind.trim() || undefined } : {}),
      }),
    );
  });
  function submit(e: FormEvent) {
    e.preventDefault();
    void create.run();
  }

  const columns: ReadonlyArray<Column<RedactionRule>> = [
    { key: "kind", header: t("common.kind"), render: (r) => <Badge tone="accent">{r.kind}</Badge> },
    { key: "pattern", header: t("redaction.pattern"), render: (r) => <span className="mono break-all">{r.pattern}</span> },
    { key: "desc", header: t("common.description"), render: (r) => r.description || "—" },
    {
      key: "status",
      header: t("common.status"),
      render: (r) => <Badge tone={r.enabled ? "ok" : "neutral"}>{r.enabled ? t("redaction.enabled") : t("redaction.disabled")}</Badge>,
    },
    { key: "updated", header: t("redaction.changed"), render: (r) => <>{time(r.updated_at)} {mono(r.updated_by)}</> },
    {
      key: "actions",
      header: "",
      className: "text-right",
      render: (r) => (
        <Button
          size="sm"
          variant={r.enabled ? "danger" : "secondary"}
          onClick={() =>
            (!r.enabled || window.confirm(t("redaction.confirmDisable", { kind: r.kind }))) &&
            void toggle.run(r)
          }
        >
          {r.enabled ? t("redaction.disable") : t("redaction.enable")}
        </Button>
      ),
    },
  ];

  const data = listing.data;
  return (
    <div className="space-y-4">
      <Card title={
          data
            ? t("redaction.departmentRulesEnabled", { count: data.department.filter((r) => r.enabled).length })
            : t("redaction.departmentRules")
        } bodyClassName="p-0">
        <ErrorBanner error={listing.error ?? create.error ?? toggle.error} onRetry={listing.reload} className="m-3" />
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
          <span className="text-xxs text-muted">
            {rich(t("redaction.applyNote"), { keep: <span className="mono">keep</span> })}
          </span>
        </form>
        <DataTable columns={columns} rows={data?.department} rowKey={(r) => r.rule_id} loading={listing.loading} empty={t("redaction.empty")} />
      </Card>

      <Card title={t("redaction.tryIt")}>
        <div className="space-y-2">
          <ErrorBanner error={test.error} />
          <TextArea value={sample} onChange={(e) => setSample(e.target.value)} rows={3} placeholder={t("redaction.samplePlaceholder")} className="w-full" />
          <div className="flex items-center gap-2">
            <Button busy={test.busy} disabled={!sample} onClick={() => void test.run()}>
              {t("redaction.test")}
            </Button>
            <span className="text-xxs text-muted">
              {pattern ? t("redaction.withPattern") : t("redaction.withRules")}
            </span>
          </div>
          {preview && (
            <>
              {preview.refused ? (
                <div role="alert" className="rounded-sm border border-err/40 bg-err/10 px-3 py-2 text-xs text-err">
                  {t("redaction.refused", { reason: preview.refused })}
                </div>
              ) : (
                preview.pattern_share != null &&
                preview.pattern_share > 0.2 && (
                  <div role="alert" className="rounded-sm border border-warn/40 bg-warn/10 px-3 py-2 text-xs">
                    {t("redaction.shareWarning", { percent: Math.round(preview.pattern_share * 100) })}
                  </div>
                )
              )}
              {preview.pattern_share != null && (
                <span className="text-xxs text-muted">{t("redaction.share", { percent: Math.round(preview.pattern_share * 100) })}</span>
              )}
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
          )}
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
