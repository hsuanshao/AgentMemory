import { useState } from "react";
import { api, type ConversationItem, type Scope } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { DataTable, type Column } from "../../components/DataTable";
import { ErrorBanner } from "../../components/Feedback";
import { Input } from "../../components/Field";
import { Pager } from "../../components/Pager";
import { SearchBox } from "../../components/SearchBox";
import { useAction } from "../../hooks/useAction";
import { useIsAdmin } from "../auth/session";
import { useAsync } from "../../hooks/useAsync";
import { formatTime } from "../../lib/format";

const PAGE = 20;

interface L0View {
  rows: ConversationItem[];
  total?: number;
}

export function L0Tab({ scope }: { scope: Scope }) {
  const [query, setQuery] = useState("");
  const [sessionId, setSessionId] = useState("");
  const [offset, setOffset] = useState(0);

  const list = useAsync<L0View>(async () => {
    const session_id = sessionId.trim() || undefined;
    if (query) return { rows: (await api.conversation.search(scope, query, 50)).messages };
    const res = await api.conversation.query(scope, { limit: PAGE, offset, session_id });
    return { rows: res.messages, total: res.total };
  }, [scope, query, sessionId, offset]);

  const remove = useAction(async (id: string) => {
    await api.conversation.delete(scope, [id]);
    await list.reload();
  });

  const admin = useIsAdmin();
  const columns: ReadonlyArray<Column<ConversationItem>> = [
    { key: "role", header: "Role", render: (m) => <Badge tone={m.role === "user" ? "accent" : "neutral"}>{m.role}</Badge> },
    { key: "content", header: "Content", render: (m) => <div className="max-w-3xl whitespace-pre-wrap wrap-break-word">{m.content}</div> },
    { key: "session", header: "Session", render: (m) => <span className="mono text-muted">{m.session_id ?? "—"}</span> },
    ...(query ? [{ key: "score", header: "Score", render: (m: ConversationItem) => <span className="mono">{m.score?.toFixed(3) ?? "—"}</span> }] : []),
    { key: "time", header: "Time", render: (m) => <span className="mono whitespace-nowrap text-muted">{formatTime(m.recorded_at ?? m.timestamp)}</span> },
    ...(admin ? [{
      key: "actions",
      header: "",
      className: "text-right",
      render: (m: ConversationItem) => (
        <Button
          size="sm"
          variant="danger"
          busy={remove.busy}
          onClick={() => window.confirm(`Delete message ${m.id}?`) && void remove.run(m.id)}
        >
          Delete
        </Button>
      ),
    }] : []),
  ];

  return (
    <Card
      title="L0 · Conversations"
      bodyClassName="p-0"
      actions={
        <>
          <Input
            value={sessionId}
            onChange={(e) => {
              setSessionId(e.target.value);
              setOffset(0);
            }}
            placeholder="session_id filter"
            className="mono w-44"
            disabled={!!query}
          />
          <SearchBox value={query} onSearch={(q) => (setQuery(q), setOffset(0))} placeholder="Keyword search" />
        </>
      }
    >
      <ErrorBanner error={list.error ?? remove.error} onRetry={list.reload} className="m-3" />
      <DataTable columns={columns} rows={list.data?.rows} rowKey={(m) => m.id} loading={list.loading} empty="No messages in this scope" />
      {!query && list.data && (
        <Pager offset={offset} limit={PAGE} total={list.data.total} count={list.data.rows.length} onChange={setOffset} />
      )}
    </Card>
  );
}
