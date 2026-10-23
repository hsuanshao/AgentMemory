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

/**
 * System admins only: the redaction rules the department's writes go through.
 * The built-in and deployment rules are listed for reference; only the
 * department's own can be added, changed or disabled here (#57).
 */
export function RedactionPanel() {
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
    { key: "kind", header: "Kind", render: (r) => <Badge tone="accent">{r.kind}</Badge> },
    { key: "pattern", header: "Pattern", render: (r) => <span className="mono break-all">{r.pattern}</span> },
    { key: "desc", header: "Description", render: (r) => r.description || "—" },
    { key: "status", header: "Status", render: (r) => <Badge tone={r.enabled ? "ok" : "neutral"}>{r.enabled ? "enabled" : "disabled"}</Badge> },
    { key: "updated", header: "Changed", render: (r) => <>{time(r.updated_at)} {mono(r.updated_by)}</> },
    {
      key: "actions",
      header: "",
      className: "text-right",
      render: (r) => (
        <Button
          size="sm"
          variant={r.enabled ? "danger" : "secondary"}
          onClick={() =>
            (!r.enabled || window.confirm(`Disable ${r.kind}? Writes from now on are no longer redacted by it; stored data is not changed.`)) &&
            void toggle.run(r)
          }
        >
          {r.enabled ? "Disable" : "Enable"}
        </Button>
      ),
    },
  ];

  const data = listing.data;
  return (
    <div className="space-y-4">
      <Card title={`Department rules${data ? ` (${data.department.filter((r) => r.enabled).length} enabled)` : ""}`} bodyClassName="p-0">
        <ErrorBanner error={listing.error ?? create.error ?? toggle.error} onRetry={listing.reload} className="m-3" />
        <form onSubmit={submit} className="space-y-2 border-b border-line p-3">
          <div className="flex flex-wrap items-end gap-2">
            <Field label="Kind">
              <Input value={kind} onChange={(e) => setKind(e.target.value)} placeholder="e.g. employee-id" className="w-44" />
            </Field>
            <Field label="Pattern (Rust regex)">
              <Input value={pattern} onChange={(e) => setPattern(e.target.value)} placeholder={String.raw`\bEMP-\d{6}\b`} className="mono w-80" />
            </Field>
            <Field label="Description">
              <Input value={description} onChange={(e) => setDescription(e.target.value)} placeholder="optional" className="w-56" />
            </Field>
            <Button type="submit" variant="primary" busy={create.busy} disabled={!kind.trim() || !pattern}>
              Add rule
            </Button>
          </div>
          <span className="text-xxs text-muted">
            Applies to writes from now on, on top of the built-in and deployment rules; stored data is not changed. Matches become
            [REDACTED:kind]. A <span className="mono">keep</span> group keeps that part of the match.
          </span>
        </form>
        <DataTable columns={columns} rows={data?.department} rowKey={(r) => r.rule_id} loading={listing.loading} empty="No department rules" />
      </Card>

      <Card title="Try it">
        <div className="space-y-2">
          <ErrorBanner error={test.error} />
          <TextArea value={sample} onChange={(e) => setSample(e.target.value)} rows={3} placeholder="Sample text; it is not stored" className="w-full" />
          <div className="flex items-center gap-2">
            <Button busy={test.busy} disabled={!sample} onClick={() => void test.run()}>
              Test
            </Button>
            <span className="text-xxs text-muted">
              {pattern ? "With the pattern above, before adding it." : "With the rules as they are."}
            </span>
          </div>
          {preview && (
            <>
              <CodeBlock>{preview.redacted}</CodeBlock>
              <div className="flex flex-wrap gap-1">
                {preview.hits.length === 0 ? (
                  <span className="text-xxs text-muted">Nothing redacted</span>
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
        <Card title="Built-in and deployment rules (read-only)">
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
              <span className="text-muted">No deployment patterns (redaction.patterns).</span>
            )}
          </div>
        </Card>
      )}
    </div>
  );
}
