import { useEffect, useState, type FormEvent } from "react";
import { api, type Scope } from "../../api";
import { Button } from "../../components/Button";
import { ErrorBanner } from "../../components/Feedback";
import { Field, Input, Select } from "../../components/Field";
import { useAsync } from "../../hooks/useAsync";
import { useStore } from "../../hooks/useStore";
import { useT } from "../../i18n";
import { isSystemAdmin, sessionStore } from "../auth/session";

const LIMIT = 100;

/**
 * Picks a scope Core will authorize: a team the viewer can see, an agent of
 * that team, and (system admins only) any user; everyone else browses their
 * own memory.
 */
export function ScopePicker({ value, onChange }: { value: Scope; onChange: (scope: Scope) => void }) {
  const t = useT();
  const session = useStore(sessionStore);
  const self = session?.user.user_id ?? "";
  const admin = isSystemAdmin(session);
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);

  const teams = useAsync(
    () => (admin ? api.admin.allTeams({ limit: LIMIT }) : api.meta.teams(self, { limit: LIMIT })),
    [self, admin],
    !!self,
  );
  const agents = useAsync(() => api.meta.agents(draft.team_id, { limit: LIMIT }), [draft.team_id], !!draft.team_id);

  // Keep the draft pointing at something that exists (e.g. after a team was deleted).
  useEffect(() => {
    const ids = teams.data?.items.map((x) => x.team_id);
    if (ids && !ids.includes(draft.team_id)) setDraft((d) => ({ ...d, team_id: ids[0] ?? "", agent_id: "" }));
  }, [teams.data, draft.team_id]);
  useEffect(() => {
    const ids = agents.data?.items.map((a) => a.agent_id);
    if (ids && !ids.includes(draft.agent_id)) setDraft((d) => ({ ...d, agent_id: ids[0] ?? "" }));
  }, [agents.data, draft.agent_id]);

  const next: Scope = { ...draft, user_id: admin ? draft.user_id.trim() || self : self };
  const dirty = next.team_id !== value.team_id || next.agent_id !== value.agent_id || next.user_id !== value.user_id;

  function apply(e: FormEvent) {
    e.preventDefault();
    onChange(next);
  }

  const teamOptions = (teams.data?.items ?? []).map((x) => ({ value: x.team_id, label: x.name }));
  const agentOptions = (agents.data?.items ?? []).map((a) => ({ value: a.agent_id, label: a.name }));

  return (
    <form onSubmit={apply} className="flex flex-wrap items-end gap-2 rounded-md border border-line bg-surface p-3">
      <Field label={t("common.team")}>
        <Select
          aria-label={t("common.team")}
          options={teamOptions.length ? teamOptions : [{ value: "", label: t("scope.noTeam") }]}
          value={draft.team_id}
          onChange={(e) => setDraft({ ...draft, team_id: e.target.value, agent_id: "" })}
          className="w-48"
        />
      </Field>
      <Field label={t("common.agent")}>
        <Select
          aria-label={t("common.agent")}
          options={agentOptions.length ? agentOptions : [{ value: "", label: t("scope.noAgent") }]}
          value={draft.agent_id}
          onChange={(e) => setDraft({ ...draft, agent_id: e.target.value })}
          className="w-48"
        />
      </Field>
      <Field label="user_id">
        <Input
          value={admin ? draft.user_id : self}
          onChange={(e) => setDraft({ ...draft, user_id: e.target.value })}
          placeholder={self}
          disabled={!admin}
          title={admin ? undefined : t("scope.ownOnly")}
          className="mono w-56"
        />
      </Field>
      <Button type="submit" variant={dirty ? "primary" : "secondary"} disabled={!next.team_id || !next.agent_id}>
        {t("scope.apply")}
      </Button>
      <ErrorBanner error={teams.error ?? agents.error} className="basis-full" />
      <p className="basis-full text-xxs text-muted">{t("scope.isolation")}</p>
    </form>
  );
}
