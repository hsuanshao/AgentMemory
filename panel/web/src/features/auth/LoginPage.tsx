import { useState, type FormEvent } from "react";
import { api, errorMessage } from "../../api";
import { Button } from "../../components/Button";
import { Field, Input } from "../../components/Field";
import { LanguageSelect } from "../../components/LanguageSelect";
import { useT } from "../../i18n";
import { rich } from "../../i18n/rich";
import { sessionStore } from "./session";

const KEY_PREFIX = "sk-mem-";

export function LoginPage() {
  const t = useT();
  const [userKey, setUserKey] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const key = userKey.trim();
    if (!key) return;
    setBusy(true);
    setError(undefined);
    try {
      // auth/verify answers HTTP 200 even for invalid keys: check data.valid.
      const result = await api.verifyUserKey(key);
      if (!result.valid || !result.user) {
        setError(t("login.invalid"));
        return;
      }
      sessionStore.set({ userKey: key, user: result.user, verifiedAt: Date.now() });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-full items-center justify-center p-6">
      <form onSubmit={submit} className="w-full max-w-sm space-y-4 rounded-md border border-line bg-surface p-6 shadow-xs">
        <div>
          <div className="flex items-center justify-between gap-2">
            <h1 className="flex items-center gap-2 text-base font-semibold">
              <img src="/favicon.svg" alt="" className="h-6 w-6" />
              Memory Panel
            </h1>
            <LanguageSelect />
          </div>
          <p className="mt-1 text-xs text-muted">{t("login.subtitle")}</p>
        </div>
        <Field label={t("login.userKey")}>
          <Input
            type="password"
            autoComplete="off"
            autoFocus
            placeholder={`${KEY_PREFIX}…`}
            value={userKey}
            onChange={(e) => setUserKey(e.target.value)}
          />
        </Field>
        {userKey && !userKey.trim().startsWith(KEY_PREFIX) && (
          <p className="text-xxs text-warn">{t("login.prefixHint", { prefix: KEY_PREFIX })}</p>
        )}
        {error && <p className="mono text-err">{error}</p>}
        <Button type="submit" variant="primary" className="w-full" busy={busy} disabled={!userKey.trim()}>
          {t("login.signIn")}
        </Button>
        <div className="rounded-sm border border-line bg-raised p-3 text-xxs leading-relaxed text-muted">
          {rich(t("login.noKey"), {
            endpoint: <code className="mono text-fg">POST /v3/internal/meta/user/init-admin</code>,
          })}
        </div>
      </form>
    </div>
  );
}
