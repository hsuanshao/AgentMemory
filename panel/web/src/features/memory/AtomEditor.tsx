import { useEffect, useState } from "react";
import { api, type AtomicDetail, type Scope } from "../../api";
import { Button } from "../../components/Button";
import { KeyValues } from "../../components/CodeBlock";
import { Drawer } from "../../components/Drawer";
import { ErrorBanner } from "../../components/Feedback";
import { Field, TextArea } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { formatTime } from "../../lib/format";
import { useT } from "../../i18n";

export function AtomEditor({
  scope,
  atom,
  onClose,
  onSaved,
}: {
  scope: Scope;
  atom: AtomicDetail | undefined;
  onClose: () => void;
  onSaved: () => void;
}) {
  const t = useT();
  const [content, setContent] = useState("");
  const [background, setBackground] = useState("");

  useEffect(() => {
    setContent(atom?.content ?? "");
    setBackground(atom?.background ?? "");
  }, [atom]);

  const save = useAction(async () => {
    if (!atom) return;
    await api.atomic.update(scope, atom.id, content, background || undefined);
    onSaved();
  });

  const unchanged = content === atom?.content && background === (atom?.background ?? "");

  return (
    <Drawer
      open={!!atom}
      title={atom ? t("atom.editTitle", { id: atom.id }) : ""}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button variant="primary" busy={save.busy} disabled={!content.trim() || unchanged} onClick={() => void save.run()}>
            {t("atom.saveNewVersion")}
          </Button>
        </>
      }
    >
      {atom && (
        <div className="space-y-4">
          <KeyValues
            entries={[
              [t("common.type"), atom.type],
              [t("common.version"), String(atom.version)],
              [t("common.created"), formatTime(atom.created_at)],
              [t("common.updated"), formatTime(atom.updated_at)],
            ]}
          />
          <Field label={t("common.content")}>
            <TextArea rows={10} maxLength={8192} value={content} onChange={(e) => setContent(e.target.value)} />
          </Field>
          <Field label={t("atom.background")}>
            <TextArea rows={4} value={background} onChange={(e) => setBackground(e.target.value)} />
          </Field>
          <ErrorBanner error={save.error} />
        </div>
      )}
    </Drawer>
  );
}
