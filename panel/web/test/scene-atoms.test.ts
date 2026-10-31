import { describe, expect, it } from "vitest";
import type { ScenarioAtom } from "../src/api/types";
import { atomStatus, reasonKey, sortAtoms } from "../src/features/memory/sceneAtoms";

const atom = (id: string, over: Partial<ScenarioAtom> = {}): ScenarioAtom => ({
  atom_id: id,
  state: "current",
  atom_version: 1,
  current_version: 1,
  content: id,
  type: "episodic",
  stale_reasons: [],
  ...over,
});

describe("atomStatus", () => {
  it("says what the scene still needs, most telling first", () => {
    expect(atomStatus(atom("a"))).toEqual({ key: "l2.atomCurrent", tone: "ok" });
    expect(atomStatus(atom("a", { current_version: 2, stale_reasons: ["rewritten"] }))).toEqual({ key: "l2.atomStale", tone: "warn" });
    expect(atomStatus(atom("a", { state: "moved_out", stale_reasons: ["moved_out"] }))).toEqual({ key: "l2.atomMovedOut", tone: "neutral" });
    expect(atomStatus(atom("a", { state: "moved_out", current_version: null, content: null }))).toEqual({ key: "l2.atomDeleted", tone: "err" });
  });
});

describe("sortAtoms", () => {
  it("lists current memories, then moved out, then deleted, keeping order within each", () => {
    const sorted = sortAtoms([
      atom("gone", { current_version: null }),
      atom("one"),
      atom("moved", { state: "moved_out" }),
      atom("two", { stale_reasons: ["rewritten"] }),
    ]);
    expect(sorted.map((a) => a.atom_id)).toEqual(["one", "two", "moved", "gone"]);
  });
});

describe("reasonKey", () => {
  it("has a message for every reason", () => {
    expect(["rewritten", "deleted", "moved_out", "changed_during_run"].map((r) => reasonKey(r as never))).toEqual([
      "l2.reasonRewritten",
      "l2.reasonDeleted",
      "l2.reasonMovedOut",
      "l2.reasonChangedDuringRun",
    ]);
  });
});
