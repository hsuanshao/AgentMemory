import { useEffect, useMemo, useState } from "react";
import { api, type Scope } from "../../api";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { cx } from "../../components/cx";
import { EmptyState, ErrorBanner, Notice, Spinner } from "../../components/Feedback";
import { Field, Input, TextArea } from "../../components/Field";
import { useAction } from "../../hooks/useAction";
import { useIsAdmin } from "../auth/session";
import { useAsync } from "../../hooks/useAsync";
import { formatTime } from "../../lib/format";
import { buildScenarioTree, type TreeRow } from "./scenarioTree";

export function L2Tab({ scope }: { scope: Scope }) {
  const admin = useIsAdmin();
  const [selected, setSelected] = useState<string>();
  const [creating, setCreating] = useState(false);
  const listing = useAsync(() => api.scenario.ls(scope), [scope]);
  const rows = useMemo(() => buildScenarioTree(listing.data?.entries ?? []), [listing.data]);

  useEffect(() => setSelected(undefined), [scope]);

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(260px,1fr)_2fr]">
      <Card
        title={`L2 · Scenarios${listing.data ? ` (${listing.data.total})` : ""}`}
        bodyClassName="p-1"
        actions={
          <>
            <Button size="sm" onClick={() => void listing.reload()}>
              ↻
            </Button>
            {admin && (
              <Button size="sm" onClick={() => (setCreating(true), setSelected(undefined))}>
                + New
              </Button>
            )}
          </>
        }
      >
        <ErrorBanner error={listing.error} onRetry={listing.reload} className="m-2" />
        {listing.loading && !listing.data && <Spinner className="m-3" />}
        {listing.data && rows.length === 0 && <EmptyState>No scenario files</EmptyState>}
        <ul className="max-h-[65vh] overflow-y-auto">
          {rows.map((row) => (
            <TreeItem key={row.path} row={row} active={row.path === selected} onSelect={() => (setSelected(row.path), setCreating(false))} />
          ))}
        </ul>
      </Card>
      {selected || creating ? (
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
      ) : (
        <Card>
          <EmptyState>Select a scenario file to read or edit it.</EmptyState>
        </Card>
      )}
    </div>
  );
}

function TreeItem({ row, active, onSelect }: { row: TreeRow; active: boolean; onSelect: () => void }) {
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
        {row.entry?.summary && <span className="ml-2 text-xxs text-muted">{row.entry.summary}</span>}
      </button>
    </li>
  );
}

function ScenarioEditor({ scope, path, onSaved }: { scope: Scope; path: string | undefined; onSaved: (path: string) => void }) {
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
      title={path ? <span className="mono normal-case">{path}</span> : "New scenario"}
      actions={
        admin && (
          <Button
            size="sm"
            variant="primary"
            busy={save.busy}
            disabled={!draftPath.trim() || draftPath.endsWith("/")}
            onClick={() => void save.run()}
          >
            Save
          </Button>
        )
      }
    >
      <div className="space-y-3">
        {file.loading && <Spinner />}
        <ErrorBanner error={file.error ?? save.error} />
        {saved && !save.busy && <Notice tone="ok">Saved.</Notice>}
        {missing && <Notice>File does not exist yet; saving will create it.</Notice>}
        {file.data && (
          <div className="mono text-muted">
            version {file.data.version ?? "—"} · created {formatTime(file.data.created_at)} · updated {formatTime(file.data.updated_at)}
          </div>
        )}
        {!path && (
          <Field label="Path">
            <Input className="mono" value={draftPath} onChange={(e) => setDraftPath(e.target.value)} placeholder="topic/scene-name.md" />
          </Field>
        )}
        <Field label="Summary (optional)">
          <Input value={summary} onChange={(e) => setSummary(e.target.value)} placeholder="Keeps the existing summary when empty" />
        </Field>
        <TextArea rows={22} value={content} onChange={(e) => (setContent(e.target.value), setSaved(false))} />
      </div>
    </Card>
  );
}
