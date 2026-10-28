import { api, type GenerationRef } from "../../api";
import { Badge } from "../../components/Badge";
import { CodeBlock, JsonView, KeyValues } from "../../components/CodeBlock";
import { Drawer } from "../../components/Drawer";
import { ErrorBanner, Spinner } from "../../components/Feedback";
import { useAsync } from "../../hooks/useAsync";
import { statusTone } from "../../lib/distillation";
import { formatTime } from "../../lib/format";
import { useT } from "../../i18n";

export function LogDetail({ logId, onClose }: { logId: string | undefined; onClose: () => void }) {
  const t = useT();
  const log = useAsync(() => api.generationLog.get(logId ?? ""), [logId], !!logId);
  const d = log.data;

  return (
    <Drawer open={!!logId} title={<span className="mono">{logId}</span>} onClose={onClose}>
      {log.loading && <Spinner />}
      <ErrorBanner error={log.error} onRetry={log.reload} />
      {d && !log.loading && (
        <div className="space-y-4">
          <KeyValues
            entries={[
              [t("logs.layerKey"), <Badge key="l">{d.layer}</Badge>],
              [t("logs.statusKey"), <Badge key="s" tone={statusTone(d.status)}>{d.status}</Badge>],
              [t("logs.scope"), [d.team_id, d.agent_id, d.user_id, d.session_id].map((v) => v ?? "·").join(" / ")],
              [t("logs.modelKey"), d.model],
              [t("logs.prompt"), d.memory_prompt_id],
              [t("logs.promptSha"), d.prompt_sha256 ? <span key="h" className="mono break-all">{d.prompt_sha256}</span> : undefined],
              [t("logs.started"), formatTime(d.started_at_ms)],
              [t("logs.finishedKey"), formatTime(d.finished_at_ms)],
              [t("logs.latencyKey"), d.latency_ms !== undefined ? `${d.latency_ms} ms` : undefined],
            ]}
          />
          {d.error_message && <CodeBlock className="border-err/40 text-err">{d.error_message}</CodeBlock>}
          <RefList title={t("logs.inputs")} refs={d.input_refs} />
          <RefList title={t("logs.outputs")} refs={d.output_refs} />
          <details>
            <summary className="label cursor-pointer">{t("logs.rawJson")}</summary>
            <JsonView value={d} className="mt-2" />
          </details>
        </div>
      )}
    </Drawer>
  );
}

function RefList({ title, refs }: { title: string; refs: GenerationRef[] | undefined }) {
  if (!refs?.length) return null;
  return (
    <div>
      <div className="label mb-1">
        {title} ({refs.length})
      </div>
      <ul className="space-y-0.5">
        {refs.map((r) => (
          <li key={`${r.layer}:${r.record_id}`} className="mono flex gap-2">
            <Badge>{r.layer}</Badge>
            <span className="break-all">{r.record_id}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
