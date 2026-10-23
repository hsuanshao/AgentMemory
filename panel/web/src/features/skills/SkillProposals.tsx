import { api, type Scope, type SkillProposal, type SkillProposalTask } from "../../api";
import { Badge } from "../../components/Badge";
import { Button } from "../../components/Button";
import { Card } from "../../components/Card";
import { CodeBlock } from "../../components/CodeBlock";
import { ErrorBanner } from "../../components/Feedback";
import { useAction } from "../../hooks/useAction";
import { useAsync } from "../../hooks/useAsync";
import { formatTime } from "../../lib/format";

/**
 * Changes skill extraction proposed for the scope's agent. Extraction reads
 * untrusted conversation text (web pages, tool output), so nothing is saved
 * until the conversation's user, a team admin or leader, or a system admin
 * approves it here. Core decides who may.
 */
export function SkillProposals({ scope, onDecided }: { scope: Scope; onDecided: () => void }) {
  const list = useAsync(() => api.skill.proposals(scope), [scope]);
  const decide = useAction(async (task: SkillProposalTask, p: SkillProposal, approve: boolean) => {
    await api.skill.decide(scope, task.task_id, p.index, approve);
    await list.reload();
    if (approve) onDecided();
  });

  const tasks = list.data?.items ?? [];
  const pending = tasks.flatMap((t) => t.proposals.filter((p) => p.state === "pending").map((p) => ({ t, p })));
  if (!list.loading && !list.error && pending.length === 0) return null;

  return (
    <Card title={`Proposed by skill extraction (${pending.length})`}>
      <div className="space-y-3">
        <ErrorBanner error={list.error ?? decide.error} onRetry={list.reload} />
        <p className="text-xxs text-muted">
          Extraction reads conversations, including web pages and tool output, so its changes wait here. Read each one before
          approving it: approved changes become the team's skills.
        </p>
        {pending.map(({ t, p }) => (
          <div key={`${t.task_id}-${p.index}`} className="space-y-2 rounded-sm border border-line p-2">
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <Badge tone="accent">{p.change.action}</Badge>
              <span className="mono">{p.change.action === "create" ? p.change.name : p.change.skill_id}</span>
              {p.base_version !== undefined && <span className="mono text-muted">from v{p.base_version}</span>}
              <span className="text-muted">
                {t.user_id} · {t.session_id} · {formatTime(t.archived_at_ms)}
              </span>
              <span className="ml-auto flex gap-1">
                <Button size="sm" variant="primary" busy={decide.busy} onClick={() => void decide.run(t, p, true)}>
                  Approve
                </Button>
                <Button size="sm" variant="secondary" busy={decide.busy} onClick={() => void decide.run(t, p, false)}>
                  Reject
                </Button>
              </span>
            </div>
            {p.change.action === "patch" ? (
              <div className="grid gap-2 md:grid-cols-2">
                <CodeBlock>{p.change.old_string}</CodeBlock>
                <CodeBlock>{p.change.new_string}</CodeBlock>
              </div>
            ) : (
              <CodeBlock>{p.change.content}</CodeBlock>
            )}
          </div>
        ))}
      </div>
    </Card>
  );
}
