import type { BadgeTone } from "../../components/Badge";
import type { MessageKey } from "../../i18n";
import type { ScenarioAtom, StaleReason } from "../../api/types";

/** How one memory of a scene is shown: a label and its tone. */
export interface AtomStatus {
  key: MessageKey;
  tone: BadgeTone;
}

const REASON_KEYS: Record<StaleReason, MessageKey> = {
  rewritten: "l2.reasonRewritten",
  deleted: "l2.reasonDeleted",
  moved_out: "l2.reasonMovedOut",
  changed_during_run: "l2.reasonChangedDuringRun",
};

export function reasonKey(reason: StaleReason): MessageKey {
  return REASON_KEYS[reason];
}

/**
 * Deleted wins over moved out, which wins over stale: each says more about
 * what the scene's text still needs than the next.
 */
export function atomStatus(atom: ScenarioAtom): AtomStatus {
  if (atom.current_version === null) return { key: "l2.atomDeleted", tone: "err" };
  if (atom.state === "moved_out") return { key: "l2.atomMovedOut", tone: "neutral" };
  if (atom.stale_reasons.length > 0) return { key: "l2.atomStale", tone: "warn" };
  return { key: "l2.atomCurrent", tone: "ok" };
}

/** Current memories first, then moved out, then deleted; stable otherwise. */
export function sortAtoms(atoms: ScenarioAtom[]): ScenarioAtom[] {
  const rank = (a: ScenarioAtom) => (a.current_version === null ? 2 : a.state === "moved_out" ? 1 : 0);
  return atoms
    .map((atom, i) => ({ atom, i }))
    .sort((a, b) => rank(a.atom) - rank(b.atom) || a.i - b.i)
    .map(({ atom }) => atom);
}
