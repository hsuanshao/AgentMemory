import { useEffect, useMemo, useState } from "react";
import { api, type Scope } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { cx } from "../../components/cx";
import { EmptyState, ErrorBanner, Notice, Spinner } from "../../components/Feedback";
import { Field, Input, TextArea } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { useIsAdmin } from "../auth/session";
import { useAsync } from "../../hooks/useAsync";
import { formatTime } from "../../lib/format";
import { useT, withCount } from "../../i18n";
import { atomStatus, reasonKey, sortAtoms } from "./sceneAtoms";
import { buildScenarioTree, type TreeRow } from "./scenarioTree";

export function L2Tab({ scope }: { scope: Scope }) {
  const t = useT();
  const admin = useIsAdmin();
  const [selected, setSelected] = useState<string>();
  const [creating, setCreating] = useState(false);
  const listing = useAsync(() => api.scenario.ls(scope), [scope]);
  const rows = useMemo(() => buildScenarioTree(listing.data?.entries ?? []), [listing.data]);

  useEffect(() => setSelected(undefined), [scope]);

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(260px,1fr)_2fr]">
      <Card
        title={withCount(t("l2.title"), listing.data?.total)}
        bodyClassName="p-1"
        actions={
          <>
            <Button size="sm" onClick={() => void listing.reload()}>
              ↻
            </Button>
            {admin && (
              <Button size="sm" onClick={() => (setCreating(true), setSelected(undefined))}>
                {t("l2.new")}
              </Button>
            )}
          </>
        }
      >
        <ErrorBanner error={listing.error} onRetry={listing.reload} className="m-2" />
        {listing.loading && !listing.data && <Spinner className="m-3" />}
        {listing.data && rows.length === 0 && <EmptyState>{t("l2.noFiles")}</EmptyState>}
        <ul className="max-h-[65vh] overflow-y-auto">
          {rows.map((row) => (
            <TreeItem key={row.path} row={row} active={row.path === selected} onSelect={() => (setSelected(row.path), setCreating(false))} />
          ))}
        </ul>
      </Card>
      {selected || creating ? (
        <div className="space-y-4">
          <ScenarioEditor
            key={selected ?? "new"}
            scope={scope}
            path={selected}
            onSaved={(path) => {
              setCreating(false);
              setSelected(path);
              void listing.reload();
            }}
          />
          {selected && <SceneAtoms key={selected} scope={scope} path={selected} stamp={listing.data} />}
        </div>
      ) : (
        <Card>
          <EmptyState>{t("l2.selectHint")}</EmptyState>
        </Card>
      )}
    </div>
  );
}

function TreeItem({ row, active, onSelect }: { row: TreeRow; active: boolean; onSelect: () => void }) {
  const t = useT();
  const indent = { paddingLeft: `${row.depth * 14 + 8}px` };
  if (row.isDir) {
    return (
      <li style={indent} className="py-1 text-xs font-medium text-muted">
        ▸ {row.name}/
      </li>
    );
  }
  return (
    <li>
      <button
        type="button"
        style={indent}
        onClick={onSelect}
        className={cx("block w-full truncate rounded-sm py-1 pr-2 text-left", active ? "bg-accent/10 text-accent" : "hover:bg-raised")}
        title={row.entry?.summary ?? row.path}
      >
        <span className="mono">{row.name}</span>
        {row.entry?.stale && (
          <Badge tone="warn" className="ml-2">
            {t("l2.stale")}
          </Badge>
        )}
        {row.entry?.summary && <span className="ml-2 text-xxs text-muted">{row.entry.summary}</span>}
      </button>
    </li>
  );
}

function ScenarioEditor({ scope, path, onSaved }: { scope: Scope; path: string | undefined; onSaved: (path: string) => void }) {
  const t = useT();
  const admin = useIsAdmin();
  const file = useAsync(() => api.scenario.read(scope, path ?? ""), [scope, path], !!path);
  const [draftPath, setDraftPath] = useState(path ?? "");
  const [content, setContent] = useState("");
  const [summary, setSummary] = useState("");
  const [saved, setSaved] = useState(false);

  useEffect(() => setContent(file.data?.content ?? ""), [file.data]);

  const save = useAction(async () => {
    const target = draftPath.trim();
    await api.scenario.write(scope, target, content, summary.trim() || undefined);
    setSaved(true);
    onSaved(target);
  });

  const missing = !!path && file.data && file.data.content === null;

  return (
    <Card
      title={path ? <span className="mono normal-case">{path}</span> : t("l2.newScenario")}
      actions={
        admin && (
          <Button
            size="sm"
            variant="primary"
            busy={save.busy}
            disabled={!draftPath.trim() || draftPath.endsWith("/")}
            onClick={() => void save.run()}
          >
            {t("common.save")}
          </Button>
        )
      }
    >
      <div className="space-y-3">
        {file.loading && <Spinner />}
        <ErrorBanner error={file.error ?? save.error} />
        {saved && !save.busy && <Notice tone="ok">{t("l2.saved")}</Notice>}
        {missing && <Notice>{t("l2.missing")}</Notice>}
        {file.data?.stale && <Notice tone="warn">{t("l2.staleNotice", { count: file.data.stale_reasons ?? 0 })}</Notice>}
        {file.data && (
          <div className="mono text-muted">
            {t("l2.meta", {
              version: file.data.version ?? "—",
              created: formatTime(file.data.created_at),
              updated: formatTime(file.data.updated_at),
            })}
          </div>
        )}
        {!path && (
          <Field label={t("l2.path")}>
            <Input className="mono" value={draftPath} onChange={(e) => setDraftPath(e.target.value)} placeholder="topic/scene-name.md" />
          </Field>
        )}
        <Field label={t("l2.summary")}>
          <Input value={summary} onChange={(e) => setSummary(e.target.value)} placeholder={t("l2.summaryPlaceholder")} />
        </Field>
        <TextArea rows={22} value={content} onChange={(e) => (setContent(e.target.value), setSaved(false))} />
      </div>
    </Card>
  );
}

/**
 * The caller's atoms this scenario was built from. Others' atoms in the
 * shared scenario are only counted. Reloads when the listing does, since a
 * save settles the scenario's stale reasons.
 */
function SceneAtoms({ scope, path, stamp }: { scope: Scope; path: string; stamp: unknown }) {
  const t = useT();
  const view = useAsync(() => api.scenario.atoms(scope, path), [scope, path, stamp]);
  const atoms = useMemo(() => sortAtoms(view.data?.atoms ?? []), [view.data]);
  return (
    <Card title={withCount(t("l2.atoms"), view.data?.atoms.length)}>
      <ErrorBanner error={view.error} onRetry={view.reload} />
      {view.loading && !view.data && <Spinner />}
      {view.data && atoms.length === 0 && <EmptyState>{t("l2.atomsEmpty")}</EmptyState>}
      <ul className="space-y-2">
        {atoms.map((atom) => {
          const status = atomStatus(atom);
          return (
            <li key={atom.atom_id} className="rounded-sm border border-line p-2">
              <div className="flex flex-wrap items-center gap-2 text-xxs text-muted">
                <Badge tone={status.tone}>{t(status.key)}</Badge>
                {atom.type && <span className="mono">{atom.type}</span>}
                {atom.current_version !== null && (
                  <span className="mono">{t("l2.atomVersions", { seen: atom.atom_version, now: atom.current_version })}</span>
                )}
                {atom.stale_reasons.map((reason) => (
                  <Badge key={reason} tone="warn">
                    {t(reasonKey(reason))}
                  </Badge>
                ))}
              </div>
              {atom.content && <p className="mt-1 whitespace-pre-wrap">{atom.content}</p>}
              <div className="mono mt-1 text-xxs text-muted">{atom.atom_id}</div>
            </li>
          );
        })}
      </ul>
      {view.data && view.data.hidden > 0 && <p className="mt-2 text-xxs text-muted">{t("l2.atomsHidden", { count: view.data.hidden })}</p>}
    </Card>
  );
}
