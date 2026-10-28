import { useState, type FormEvent } from "react";
import { api, type RecallMemory, type RecallResult, type RecallSource, type Scope } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { CodeBlock } from "../../components/CodeBlock";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Field, Input } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { formatTime } from "../../lib/format";
import { useT, withCount, type Translate } from "../../i18n";

const columns = (t: Translate): ReadonlyArray<Column<RecallMemory>> => [
  { key: "score", header: t("common.score"), render: (m) => <span className="mono">{m.score.toFixed(4)}</span> },
  { key: "type", header: t("common.type"), render: (m) => <Badge tone="accent">{m.type}</Badge> },
  {
    key: "content",
    header: t("common.content"),
    render: (m) => (
      <div className="max-w-3xl">
        <div className="whitespace-pre-wrap wrap-break-word">{m.content}</div>
        {(m.sources?.length ?? 0) + (m.sources_omitted ?? 0) > 0 && (
          <Sources sources={m.sources ?? []} omitted={m.sources_omitted ?? 0} />
        )}
      </div>
    ),
  },
  { key: "id", header: "ID", render: (m) => <span className="mono text-muted">{m.id}</span> },
];

/** The conversation a memory was distilled from, oldest first. */
function Sources({ sources, omitted }: { sources: RecallSource[]; omitted: number }) {
  const t = useT();
  return (
    <div className="mt-2 space-y-1 border-l-2 border-line pl-2 text-xs">
      <div className="label">{t("recall.sourcesTitle")}</div>
      {sources.map((s) => (
        <div key={s.id}>
          <span className="mono text-muted">
            {formatTime(s.timestamp)} · {s.role}
          </span>
          <div className="whitespace-pre-wrap wrap-break-word">
            {s.content}
            {s.truncated && <span className="text-muted"> {t("recall.sourceCut")}</span>}
          </div>
        </div>
      ))}
      {omitted > 0 && <div className="text-muted">{t("recall.sourcesOmitted", { count: omitted })}</div>}
    </div>
  );
}

export function RecallPanel({ scope }: { scope: Scope }) {
  const t = useT();
  const [query, setQuery] = useState("");
  const [maxResults, setMaxResults] = useState("10");
  const [includeSources, setIncludeSources] = useState(false);
  const [result, setResult] = useState<RecallResult>();

  const recall = useAction(async () => {
    const max = Number(maxResults);
    setResult(await api.recall(scope, query.trim(), Number.isInteger(max) && max > 0 ? max : undefined, includeSources));
  });

  function submit(e: FormEvent) {
    e.preventDefault();
    if (query.trim()) void recall.run();
  }

  return (
    <div className="space-y-4">
      <Card title={t("recall.title")}>
        <form onSubmit={submit} className="flex flex-wrap items-end gap-2">
          <Field label={t("recall.query")} className="min-w-[320px] flex-1">
            <Input value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("recall.queryPlaceholder")} />
          </Field>
          <Field label="max_results">
            <Input type="number" min={1} max={100} value={maxResults} onChange={(e) => setMaxResults(e.target.value)} className="w-24" />
          </Field>
          <label className="flex h-8 items-center gap-1 text-xs text-muted" title={t("recall.includeSourcesHint")}>
            <input type="checkbox" checked={includeSources} onChange={(e) => setIncludeSources(e.target.checked)} />
            {t("recall.includeSources")}
          </label>
          <Button type="submit" variant="primary" busy={recall.busy} disabled={!query.trim()}>
            {t("recall.run")}
          </Button>
        </form>
        <ErrorBanner error={recall.error} className="mt-3" />
      </Card>
      {result && (
        <>
          <Card title={withCount(t("recall.memories"), result.memories.length)} actions={<Badge>{t("recall.strategy", { strategy: result.strategy })}</Badge>} bodyClassName="p-0">
            <DataTable columns={columns(t)} rows={result.memories} rowKey={(m) => m.id} empty={t("recall.nothing")} />
          </Card>
          <div className="grid gap-4 xl:grid-cols-2">
            <Card title="prepend_context">
              <CodeBlock>{result.prepend_context || t("common.empty")}</CodeBlock>
            </Card>
            <Card title="append_system_context">
              <CodeBlock>{result.append_system_context || t("common.empty")}</CodeBlock>
            </Card>
          </div>
        </>
      )}
    </div>
  );
}
