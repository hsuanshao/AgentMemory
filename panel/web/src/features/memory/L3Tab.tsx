import { useEffect, useState } from "react";
import { api, type Scope } from "../../api";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { CodeBlock } from "../../components/CodeBlock";
import { EmptyState, ErrorBanner, Notice, Spinner } from "../../components/Feedback";
import { TextArea } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { useIsAdmin } from "../auth/session";
import { useAsync } from "../../hooks/useAsync";
import { formatTime } from "../../lib/format";
import { useT } from "../../i18n";

export function L3Tab({ scope }: { scope: Scope }) {
  const t = useT();
  const admin = useIsAdmin();
  const persona = useAsync(() => api.core.read(scope), [scope]);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  useEffect(() => setEditing(false), [scope]);

  const save = useAction(async () => {
    await api.core.write(scope, draft);
    setEditing(false);
    await persona.reload();
  });

  const content = persona.data?.content ?? null;

  function startEdit() {
    setDraft(content ?? "");
    setEditing(true);
  }

  return (
    <Card
      title={t("l3.title")}
      actions={
        editing ? (
          <>
            <Button size="sm" variant="ghost" onClick={() => setEditing(false)}>
              {t("common.cancel")}
            </Button>
            <Button size="sm" variant="primary" busy={save.busy} onClick={() => void save.run()}>
              {t("common.save")}
            </Button>
          </>
        ) : (
          admin && (
            <Button size="sm" onClick={startEdit} disabled={persona.loading}>
              {content === null ? t("common.create") : t("common.edit")}
            </Button>
          )
        )
      }
    >
      <div className="space-y-3">
        {persona.loading && <Spinner />}
        <ErrorBanner error={persona.error ?? save.error} onRetry={persona.reload} />
        {persona.data && (
          <div className="mono text-muted">
            {t("l3.meta", { version: persona.data.version ?? "—", updated: formatTime(persona.data.updated_at) })}
          </div>
        )}
        {editing ? (
          <>
            <Notice>{t("l3.stripNotice")}</Notice>
            <TextArea rows={26} value={draft} onChange={(e) => setDraft(e.target.value)} />
          </>
        ) : content !== null ? (
          <CodeBlock className="max-h-[70vh]">{content}</CodeBlock>
        ) : (
          persona.data && <EmptyState>{t("l3.noPersona")}</EmptyState>
        )}
      </div>
    </Card>
  );
}
