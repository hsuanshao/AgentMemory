import { useState } from "react";
import type { UserPublic } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { formatTime } from "../../lib/format";
import { useT } from "../../i18n";

export const LIST_LIMIT = 100;

export const mono = (v: string | null | undefined) => <span className="mono">{v ?? "—"}</span>;
export const statusBadge = (s: string) => <Badge tone={s === "active" ? "ok" : "neutral"}>{s}</Badge>;
export const time = (v: string | null | undefined) => <span className="mono whitespace-nowrap text-muted">{formatTime(v)}</span>;

export const ROLE_OPTIONS = [
  { value: "member", label: "member" },
  { value: "leader", label: "leader" },
  { value: "admin", label: "admin" },
] as const;

export function userOptions(users: ReadonlyArray<UserPublic> | undefined, placeholder: string) {
  return [{ value: "", label: placeholder }, ...(users ?? []).map((u) => ({ value: u.user_id, label: `${u.username} (${u.user_id})` }))];
}

/** A secret Core returns exactly once (a new user key): shown until dismissed. */
export function SecretNotice({ label, secret, onDismiss }: { label: string; secret: string; onDismiss: () => void }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(secret);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }
  return (
    <div role="status" className="space-y-1.5 rounded-sm border border-ok/40 bg-ok/10 px-3 py-2 text-xs">
      <div className="font-medium text-ok">{t("secret.notice", { label })}</div>
      <div className="flex flex-wrap items-center gap-2">
        <code className="mono break-all rounded-sm bg-surface px-1.5 py-0.5">{secret}</code>
        <Button size="sm" onClick={() => void copy()}>
          {copied ? t("secret.copied") : t("secret.copy")}
        </Button>
        <Button size="sm" variant="ghost" onClick={onDismiss}>
          {t("secret.dismiss")}
        </Button>
      </div>
    </div>
  );
}
