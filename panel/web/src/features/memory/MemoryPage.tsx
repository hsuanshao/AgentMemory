import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { EmptyState } from "../../components/Feedback";
import { PageHeader } from "../../components/PageHeader";
import { Tabs, type TabItem } from "../../components/Tabs";
import { useStore } from "../../hooks/useStore";
import { L0Tab } from "./L0Tab";
import { L1Tab } from "./L1Tab";
import { L2Tab } from "./L2Tab";
import { L3Tab } from "./L3Tab";
import { RecallPanel } from "./RecallPanel";
import { fromLink, isComplete, type MemoryTab, scopeKey, scopeStore } from "./scope";
import { ScopePicker } from "./ScopePicker";

const TABS: ReadonlyArray<TabItem<MemoryTab>> = [
  { id: "l0", label: "L0 Conversations" },
  { id: "l1", label: "L1 Atoms" },
  { id: "l2", label: "L2 Scenarios" },
  { id: "l3", label: "L3 Persona" },
  { id: "recall", label: "Recall test" },
];

export function MemoryPage() {
  const scope = useStore(scopeStore);
  const [params, setParams] = useSearchParams();
  const [tab, setTab] = useState<MemoryTab>(() => fromLink(params)?.tab ?? "l1");

  // A link (from the scope table) selects its scope once, then leaves the URL.
  useEffect(() => {
    const link = fromLink(params);
    if (!link) return;
    scopeStore.set(link.scope);
    if (link.tab) setTab(link.tab);
    setParams({}, { replace: true });
  }, [params, setParams]);

  return (
    <div className="space-y-4">
      <PageHeader title="Memory browser" subtitle={<span className="mono">scope {scopeKey(scope)}</span>} />
      <ScopePicker value={scope} onChange={scopeStore.set} />
      {isComplete(scope) ? (
        <>
          <Tabs items={TABS} value={tab} onChange={setTab} />
          {tab === "l0" && <L0Tab scope={scope} />}
          {tab === "l1" && <L1Tab scope={scope} />}
          {tab === "l2" && <L2Tab scope={scope} />}
          {tab === "l3" && <L3Tab scope={scope} />}
          {tab === "recall" && <RecallPanel scope={scope} />}
        </>
      ) : (
        <EmptyState>Pick a team and an agent, then apply the scope.</EmptyState>
      )}
    </div>
  );
}
