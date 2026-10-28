import { useState, type FormEvent } from "react";
import { api, type UserKeyInfo, type UserPublic } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Field, Input } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { useAsync } from "../../hooks/useAsync";
import { LIST_LIMIT, mono, SecretNotice, statusBadge, time } from "./shared";
import { useT, withCount } from "../../i18n";
import { rich } from "../../i18n/rich";

/** System admins only: users of the department and their keys. */
export function UsersPanel() {
  const t = useT();
  const users = useAsync(() => api.admin.users({ limit: LIST_LIMIT }), []);
  const [selected, setSelected] = useState<UserPublic>();
  const [username, setUsername] = useState("");
  const [secret, setSecret] = useState<{ label: string; value: string }>();

  const create = useAction(async () => {
    const user = await api.admin.createUser(username.trim());
    setUsername("");
    setSecret({ label: t("users.keyFor", { name: user.username }), value: user.default_user_key });
    await users.reload();
  });
  const remove = useAction(async (user: UserPublic) => {
    await api.admin.deleteUser(user.user_id);
    if (selected?.user_id === user.user_id) setSelected(undefined);
    await users.reload();
  });
  function submit(e: FormEvent) {
    e.preventDefault();
    void create.run();
  }

  const columns: ReadonlyArray<Column<UserPublic>> = [
    { key: "name", header: t("common.user"), render: (u) => <span className="font-medium">{u.username}</span> },
    { key: "id", header: "user_id", render: (u) => mono(u.user_id) },
    { key: "type", header: t("common.type"), render: (u) => <Badge tone={u.user_type === "system_admin" ? "accent" : "neutral"}>{u.user_type}</Badge> },
    { key: "created", header: t("common.created"), render: (u) => time(u.created_at) },
    {
      key: "actions",
      header: "",
      className: "text-right",
      render: (u) =>
        u.user_type !== "system_admin" && (
          <Button
            size="sm"
            variant="danger"
            onClick={(e) => {
              e.stopPropagation();
              if (window.confirm(t("users.confirmDelete", { name: u.username }))) void remove.run(u);
            }}
          >
            {t("common.delete")}
          </Button>
        ),
    },
  ];

  return (
    <div className="space-y-4">
      {secret && <SecretNotice label={secret.label} secret={secret.value} onDismiss={() => setSecret(undefined)} />}
      <Card title={withCount(t("users.title"), users.data?.total)} bodyClassName="p-0">
        <ErrorBanner error={users.error ?? create.error ?? remove.error} onRetry={users.reload} className="m-3" />
        <form onSubmit={submit} className="flex flex-wrap items-end gap-2 border-b border-line p-3">
          <Field label={t("users.new")}>
            <Input value={username} onChange={(e) => setUsername(e.target.value)} placeholder={t("users.usernamePlaceholder")} className="w-56" />
          </Field>
          <Button type="submit" variant="primary" busy={create.busy} disabled={!username.trim()}>
            {t("users.createUser")}
          </Button>
          <span className="text-xxs text-muted">{t("users.note")}</span>
        </form>
        <DataTable
          columns={columns}
          rows={users.data?.items}
          rowKey={(u) => u.user_id}
          loading={users.loading}
          empty={t("users.empty")}
          onRowClick={setSelected}
          selectedKey={selected?.user_id}
        />
      </Card>
      {selected && <KeysCard key={selected.user_id} user={selected} onSecret={(value) => setSecret({ label: t("users.newKeyFor", { name: selected.username }), value })} />}
    </div>
  );
}

function KeysCard({ user, onSecret }: { user: UserPublic; onSecret: (value: string) => void }) {
  const t = useT();
  const keys = useAsync(() => api.admin.keys(user.user_id, { limit: LIST_LIMIT }), [user.user_id]);
  const [name, setName] = useState("");
  const create = useAction(async () => {
    const key = await api.admin.createKey(user.user_id, name.trim() || undefined);
    setName("");
    if (key.key_value) onSecret(key.key_value);
    await keys.reload();
  });
  const revoke = useAction(async (keyId: string) => {
    await api.admin.revokeKey(keyId);
    await keys.reload();
  });

  const columns: ReadonlyArray<Column<UserKeyInfo>> = [
    { key: "name", header: t("common.name"), render: (k) => <span className="font-medium">{k.name ?? "—"}</span> },
    { key: "key", header: t("keys.key"), render: (k) => mono(k.key_prefix) },
    { key: "status", header: t("common.status"), render: (k) => statusBadge(k.status) },
    { key: "used", header: t("keys.lastUsed"), render: (k) => time(k.last_used_at) },
    { key: "created", header: t("common.created"), render: (k) => time(k.created_at) },
    {
      key: "actions",
      header: "",
      className: "text-right",
      render: (k) =>
        k.status === "active" && (
          <Button size="sm" variant="danger" onClick={() => window.confirm(t("keys.confirmRevoke", { prefix: k.key_prefix })) && void revoke.run(k.key_id)}>
            {t("keys.revoke")}
          </Button>
        ),
    },
  ];

  return (
    <Card title={rich(t("keys.title"), { name: <span className="normal-case">{user.username}</span> })} bodyClassName="p-0">
      <ErrorBanner error={keys.error ?? create.error ?? revoke.error} className="m-3" />
      <div className="flex flex-wrap items-end gap-2 border-b border-line p-3">
        <Field label={t("keys.new")}>
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("keys.namePlaceholder")} className="w-48" />
        </Field>
        <Button variant="primary" busy={create.busy} onClick={() => void create.run()}>
          {t("keys.createKey")}
        </Button>
      </div>
      <DataTable columns={columns} rows={keys.data?.items} rowKey={(k) => k.key_id} loading={keys.loading} empty={t("keys.empty")} />
    </Card>
  );
}
