import { useEffect, useState } from "react";
import { api, type RedactionLevel } from "../../api";
import { Field, Select } from "../../components/Field";
import { PageHeader } from "../../components/PageHeader";
import { Tabs, type TabItem } from "../../components/Tabs";
import { useAsync } from "../../hooks/useAsync";
import { useStore } from "../../hooks/useStore";
import { useT } from "../../i18n";
import { isSystemAdmin, sessionStore } from "../auth/session";
import { LIST_LIMIT } from "../meta/shared";
import { RedactionRules } from "./RedactionRules";

type LevelTab = "all" | "team" | "user";

/**
 * Redaction rules by level: every team, one team the viewer belongs to (any
 * team for system admins), and the viewer's own (any user's for system
 * admins). Core decides what each viewer may read and change.
 */
export function RedactionPage() {
  const t = useT();
  const session = useStore(sessionStore);
  const self = session?.user.user_id ?? "";
  const admin = isSystemAdmin(session);
  const [tab, setTab] = useState<LevelTab>("all");
  const [teamId, setTeamId] = useState("");
  const [userId, setUserId] = useState(self);

  const teams = useAsync(
    () => (admin ? api.admin.allTeams({ limit: LIST_LIMIT }) : api.meta.teams(self, { limit: LIST_LIMIT })),
    [self, admin],
    !!self,
  );
  const users = useAsync(() => api.admin.users({ limit: LIST_LIMIT }), [], admin);

  // Keep the team pointing at one that exists.
  useEffect(() => {
    const ids = teams.data?.items.map((x) => x.team_id);
    if (ids && !ids.includes(teamId)) setTeamId(ids[0] ?? "");
  }, [teams.data, teamId]);

  const tabs: TabItem<LevelTab>[] = [
    { id: "all", label: t("redaction.tabAll") },
    { id: "team", label: t("redaction.tabTeam") },
    { id: "user", label: t("redaction.tabUser") },
  ];
  const teamOptions = (teams.data?.items ?? []).map((x) => ({ value: x.team_id, label: x.name }));
  const userOptions = (users.data?.items ?? []).map((u) => ({ value: u.user_id, label: `${u.username} (${u.user_id})` }));
  const teamName = teams.data?.items.find((x) => x.team_id === teamId)?.name ?? teamId;
  const userName = userId === self ? (session?.user.username ?? self) : (users.data?.items.find((u) => u.user_id === userId)?.username ?? userId);

  let level: RedactionLevel | undefined = {};
  let title = t("redaction.allRules");
  if (tab === "team") {
    level = teamId ? { team_id: teamId } : undefined;
    title = t("redaction.teamRules", { team: teamName });
  } else if (tab === "user") {
    level = { user_id: userId || self };
    title = t("redaction.userRules", { user: userName });
  }

  return (
    <div className="space-y-4">
      <PageHeader title={t("redaction.title")} subtitle={t("redaction.subtitle")} />
      <Tabs items={tabs} value={tab} onChange={setTab} />
      {tab === "team" && (
        <Field label={t("common.team")} className="w-56">
          <Select
            aria-label={t("common.team")}
            options={teamOptions.length ? teamOptions : [{ value: "", label: t("scope.noTeam") }]}
            value={teamId}
            onChange={(e) => setTeamId(e.target.value)}
          />
        </Field>
      )}
      {tab === "user" && admin && (
        <Field label={t("common.user")} className="w-72">
          <Select aria-label={t("common.user")} options={userOptions} value={userId} onChange={(e) => setUserId(e.target.value)} />
        </Field>
      )}
      {level && (
        <RedactionRules
          key={`${tab}:${level.team_id ?? ""}:${level.user_id ?? ""}`}
          level={level}
          title={title}
          teams={tab === "user" ? teamOptions : undefined}
        />
      )}
    </div>
  );
}
