import { useEffect, useState, type FormEvent } from "react";
import { api, type Agent, type Team, type TeamMember, type TeamRole } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Field, Input, Select } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { useAsync } from "../../hooks/useAsync";
import { useStore } from "../../hooks/useStore";
import { sessionStore } from "../auth/session";
import { LIST_LIMIT, mono, ROLE_OPTIONS, statusBadge, time, userOptions } from "./shared";
import { useT, withCount, type Translate } from "../../i18n";
import { rich } from "../../i18n/rich";

const teamColumns = (t: Translate): ReadonlyArray<Column<Team>> => [
  { key: "name", header: t("common.team"), render: (team) => <span className="font-medium">{team.name}</span> },
  { key: "id", header: "team_id", render: (team) => mono(team.team_id) },
  { key: "owner", header: t("common.owner"), render: (team) => mono(team.owner_user_id) },
  { key: "status", header: t("common.status"), render: (team) => statusBadge(team.status) },
];

/** Teams the viewer can see; admins see all of them and get the editing controls. */
export function TeamsPanel({ userId, admin }: { userId: string; admin: boolean }) {
  const t = useT();
  const [teamId, setTeamId] = useState<string>();
  const teams = useAsync(
    () => (admin ? api.admin.allTeams({ limit: LIST_LIMIT }) : api.meta.teams(userId, { limit: LIST_LIMIT })),
    [userId, admin],
    !!userId,
  );
  const users = useAsync(() => api.admin.users({ limit: LIST_LIMIT }), [], admin);
  useEffect(() => {
    const ids = teams.data?.items.map((x) => x.team_id) ?? [];
    setTeamId((current) => (current && ids.includes(current) ? current : ids[0]));
  }, [teams.data]);
  const team = teams.data?.items.find((x) => x.team_id === teamId);

  return (
    <div className="space-y-4">
      <Card title={withCount(t("teams.title"), teams.data?.total)} bodyClassName="p-0">
        <ErrorBanner error={teams.error} onRetry={teams.reload} className="m-3" />
        {admin && <NewTeamForm users={users.data?.items} onCreated={(created) => void teams.reload().then(() => setTeamId(created.team_id))} />}
        <DataTable
          columns={teamColumns(t)}
          rows={teams.data?.items}
          rowKey={(x) => x.team_id}
          loading={teams.loading}
          empty={admin ? t("teams.emptyAdmin") : t("teams.emptyUser")}
          onRowClick={(x) => setTeamId(x.team_id)}
          selectedKey={teamId}
        />
      </Card>
      {team && admin && <TeamSettings key={team.team_id} team={team} onChanged={teams.reload} />}
      {team && (
        <div className="grid gap-4 xl:grid-cols-2">
          <MembersCard key={`m-${team.team_id}`} team={team} admin={admin} allUsers={users.data?.items} />
          <AgentsCard key={`a-${team.team_id}`} team={team} admin={admin} />
        </div>
      )}
    </div>
  );
}

function NewTeamForm({ users, onCreated }: { users: Parameters<typeof userOptions>[0]; onCreated: (team: Team) => void }) {
  const t = useT();
  const [name, setName] = useState("");
  const [owner, setOwner] = useState("");
  const [description, setDescription] = useState("");
  const create = useAction(async () => {
    const team = await api.admin.createTeam({ name: name.trim(), owner_user_id: owner, description: description.trim() || undefined });
    setName("");
    setDescription("");
    onCreated(team);
  });
  function submit(e: FormEvent) {
    e.preventDefault();
    void create.run();
  }
  return (
    <form onSubmit={submit} className="flex flex-wrap items-end gap-2 border-b border-line p-3">
      <Field label={t("teams.new")}>
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("teams.namePlaceholder")} className="w-44" />
      </Field>
      <Field label={t("common.owner")}>
        <Select aria-label={t("common.owner")} options={userOptions(users, t("common.selectOwner"))} value={owner} onChange={(e) => setOwner(e.target.value)} />
      </Field>
      <Field label={t("common.description")}>
        <Input value={description} onChange={(e) => setDescription(e.target.value)} placeholder={t("common.optional")} className="w-56" />
      </Field>
      <Button type="submit" variant="primary" busy={create.busy} disabled={!name.trim() || !owner}>
        {t("teams.createTeam")}
      </Button>
      <ErrorBanner error={create.error} className="basis-full" />
    </form>
  );
}

function TeamSettings({ team, onChanged }: { team: Team; onChanged: () => Promise<void> }) {
  const t = useT();
  const [name, setName] = useState(team.name);
  const [description, setDescription] = useState(team.description ?? "");
  const save = useAction(async () => {
    await api.admin.updateTeam({ team_id: team.team_id, name: name.trim(), description });
    await onChanged();
  });
  const toggle = useAction(async () => {
    await api.admin.updateTeam({ team_id: team.team_id, status: team.status === "active" ? "archived" : "active" });
    await onChanged();
  });
  const remove = useAction(async () => {
    await api.admin.deleteTeam(team.team_id);
    await onChanged();
  });
  const dirty = name.trim() !== team.name || description !== (team.description ?? "");

  return (
    <Card title={rich(t("teams.settings"), { id: <span className="mono normal-case">{team.team_id}</span> })}>
      <div className="flex flex-wrap items-end gap-2">
        <Field label={t("common.name")}>
          <Input value={name} onChange={(e) => setName(e.target.value)} className="w-48" />
        </Field>
        <Field label={t("common.description")}>
          <Input value={description} onChange={(e) => setDescription(e.target.value)} className="w-72" />
        </Field>
        <Button variant="primary" busy={save.busy} disabled={!dirty || !name.trim()} onClick={() => void save.run()}>
          {t("common.save")}
        </Button>
        <Button busy={toggle.busy} onClick={() => void toggle.run()}>
          {team.status === "active" ? t("teams.archive") : t("teams.reactivate")}
        </Button>
        <Button
          variant="danger"
          busy={remove.busy}
          onClick={() =>
            window.confirm(t("teams.confirmDelete", { name: team.name })) && void remove.run()
          }
        >
          {t("teams.deleteTeam")}
        </Button>
      </div>
      <ErrorBanner error={save.error ?? toggle.error ?? remove.error} className="mt-2" />
    </Card>
  );
}

function MembersCard({ team, admin, allUsers }: { team: Team; admin: boolean; allUsers: Parameters<typeof userOptions>[0] }) {
  const t = useT();
  const self = useStore(sessionStore)?.user.user_id;
  const members = useAsync(() => api.meta.members(team.team_id, { limit: LIST_LIMIT }), [team.team_id]);
  const [userId, setUserId] = useState("");
  const [role, setRole] = useState<TeamRole>("member");
  const add = useAction(async (id: string, r: TeamRole) => {
    await api.admin.addMember(team.team_id, id, r);
    setUserId("");
    await members.reload();
  });
  const remove = useAction(async (id: string) => {
    await api.admin.removeMember(team.team_id, id);
    await members.reload();
  });

  const memberIds = new Set(members.data?.items.filter((m) => m.status === "active").map((m) => m.user_id));
  // Core refuses adding yourself, so the signed-in admin is not offered.
  const candidates = allUsers?.filter((u) => !memberIds.has(u.user_id) && u.user_id !== self);
  const columns: Column<TeamMember>[] = [
    { key: "user", header: t("common.user"), render: (m) => <span className="font-medium">{m.username ?? m.user_id}</span> },
    { key: "id", header: "user_id", render: (m) => mono(m.user_id) },
    {
      key: "role",
      header: t("common.role"),
      render: (m) =>
        admin && m.status === "active" && m.user_id !== team.owner_user_id ? (
          <Select
            aria-label={t("members.roleOf", { user: m.user_id })}
            options={ROLE_OPTIONS}
            value={m.role}
            disabled={add.busy}
            onChange={(e) => void add.run(m.user_id, e.target.value as TeamRole)}
          />
        ) : (
          <Badge tone={m.role === "admin" ? "accent" : "neutral"}>{m.role}</Badge>
        ),
    },
    { key: "status", header: t("common.status"), render: (m) => statusBadge(m.status) },
    { key: "joined", header: t("members.joined"), render: (m) => time(m.joined_at) },
  ];
  if (admin) {
    columns.push({
      key: "actions",
      header: "",
      className: "text-right",
      render: (m) =>
        m.status === "active" &&
        m.user_id !== team.owner_user_id && (
          <Button size="sm" variant="danger" onClick={() => window.confirm(t("members.confirmRemove", { user: m.username ?? m.user_id })) && void remove.run(m.user_id)}>
            {t("common.remove")}
          </Button>
        ),
    });
  }

  return (
    <Card title={withCount(t("members.title"), members.data?.total)} bodyClassName="p-0">
      <ErrorBanner error={members.error ?? add.error ?? remove.error} className="m-3" />
      {admin && (
        <div className="flex flex-wrap items-end gap-2 border-b border-line p-3">
          <Field label={t("members.assign")}>
            <Select aria-label={t("common.user")} options={userOptions(candidates, t("members.selectUser"))} value={userId} onChange={(e) => setUserId(e.target.value)} />
          </Field>
          <Field label={t("common.role")}>
            <Select aria-label={t("common.role")} options={ROLE_OPTIONS} value={role} onChange={(e) => setRole(e.target.value as TeamRole)} />
          </Field>
          <Button variant="primary" busy={add.busy} disabled={!userId} onClick={() => void add.run(userId, role)}>
            {t("common.add")}
          </Button>
        </div>
      )}
      <DataTable columns={columns} rows={members.data?.items} rowKey={(m) => m.id} loading={members.loading} empty={t("members.empty")} />
    </Card>
  );
}

function AgentsCard({ team, admin }: { team: Team; admin: boolean }) {
  const t = useT();
  const agents = useAsync(() => api.meta.agents(team.team_id, { limit: LIST_LIMIT }), [team.team_id]);
  const members = useAsync(() => api.meta.members(team.team_id, { limit: LIST_LIMIT }), [team.team_id], admin);
  const [name, setName] = useState("");
  const [owner, setOwner] = useState("");
  const create = useAction(async () => {
    await api.admin.createAgent({ team_id: team.team_id, owner_user_id: owner, name: name.trim() });
    setName("");
    await agents.reload();
  });
  const remove = useAction(async (id: string) => {
    await api.admin.deleteAgent(id);
    await agents.reload();
  });

  const owners = members.data?.items
    .filter((m) => m.status === "active")
    .map((m) => ({ value: m.user_id, label: `${m.username ?? m.user_id}` }));
  const columns: Column<Agent>[] = [
    { key: "name", header: t("common.agent"), render: (a) => <span className="font-medium">{a.name}</span> },
    { key: "id", header: "agent_id", render: (a) => mono(a.agent_id) },
    { key: "owner", header: t("common.owner"), render: (a) => mono(a.owner_user_id) },
    { key: "status", header: t("common.status"), render: (a) => statusBadge(a.status) },
  ];
  if (admin) {
    columns.push({
      key: "actions",
      header: "",
      className: "text-right",
      render: (a) => (
        <Button size="sm" variant="danger" onClick={() => window.confirm(t("agents.confirmDelete", { name: a.name })) && void remove.run(a.agent_id)}>
          {t("common.delete")}
        </Button>
      ),
    });
  }

  return (
    <Card title={withCount(t("agents.title"), agents.data?.total)} bodyClassName="p-0">
      <ErrorBanner error={agents.error ?? create.error ?? remove.error} className="m-3" />
      {admin && (
        <div className="flex flex-wrap items-end gap-2 border-b border-line p-3">
          <Field label={t("agents.new")}>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("agents.namePlaceholder")} className="w-40" />
          </Field>
          <Field label={t("common.owner")}>
            <Select aria-label={t("agents.ownerLabel")} options={[{ value: "", label: t("common.selectOwner") }, ...(owners ?? [])]} value={owner} onChange={(e) => setOwner(e.target.value)} />
          </Field>
          <Button variant="primary" busy={create.busy} disabled={!name.trim() || !owner} onClick={() => void create.run()}>
            {t("common.create")}
          </Button>
        </div>
      )}
      <DataTable columns={columns} rows={agents.data?.items} rowKey={(a) => a.agent_id} loading={agents.loading} empty={t("agents.empty")} />
    </Card>
  );
}
