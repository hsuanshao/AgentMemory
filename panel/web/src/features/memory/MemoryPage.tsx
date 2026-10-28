import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { EmptyState } from "../../components/Feedback";
import { PageHeader } from "../../components/PageHeader";
import { Tabs, type TabItem } from "../../components/Tabs";
import { useStore } from "../../hooks/useStore";
import { useT, type Translate } from "../../i18n";
import { L0Tab } from "./L0Tab";
import { L1Tab } from "./L1Tab";
import { L2Tab } from "./L2Tab";
import { L3Tab } from "./L3Tab";
import { RecallPanel } from "./RecallPanel";
import { fromLink, isComplete, type MemoryTab, scopeKey, scopeStore } from "./scope";
import { ScopePicker } from "./ScopePicker";

const tabs = (t: Translate): ReadonlyArray<TabItem<MemoryTab>> => [
  { id: "l0", label: t("memory.tabL0") },
  { id: "l1", label: t("memory.tabL1") },
  { id: "l2", label: t("memory.tabL2") },
  { id: "l3", label: t("memory.tabL3") },
  { id: "recall", label: t("memory.tabRecall") },
];

export function MemoryPage() {
  const t = useT();
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
      <PageHeader title={t("memory.title")} subtitle={<span className="mono">{t("memory.scope", { scope: scopeKey(scope) })}</span>} />
      <ScopePicker value={scope} onChange={scopeStore.set} />
      {isComplete(scope) ? (
        <>
          <Tabs items={tabs(t)} value={tab} onChange={setTab} />
          {tab === "l0" && <L0Tab scope={scope} />}
          {tab === "l1" && <L1Tab scope={scope} />}
          {tab === "l2" && <L2Tab scope={scope} />}
          {tab === "l3" && <L3Tab scope={scope} />}
          {tab === "recall" && <RecallPanel scope={scope} />}
        </>
      ) : (
        <EmptyState>{t("memory.pickScope")}</EmptyState>
      )}
    </div>
  );
}
