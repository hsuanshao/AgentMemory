import { describe, expect, it } from "vitest";
import type { DeadLetter } from "../src/api/types";
import { countByKind, deadLetterSubject, layerRows, statusTone } from "../src/lib/distillation";

describe("statusTone", () => {
  it("colours failures and successes, leaves empty runs neutral", () => {
    expect(statusTone("succeeded")).toBe("ok");
    expect(statusTone("failed")).toBe("err");
    expect(statusTone("empty")).toBe("neutral");
  });
});

describe("layerRows", () => {
  it("zero-fills every layer and computes the failure rate", () => {
    const rows = layerRows({
      hours: 24,
      from_ms: 0,
      llm: { configured: true, model: "cli:agy:gemini-3.8-flash-high" },
      items: [
        { layer: "l1", status: "succeeded", count: 6 },
        { layer: "l1", status: "empty", count: 2 },
        { layer: "l1", status: "failed", count: 2 },
        { layer: "l3", status: "failed", count: 1 },
      ],
    });
    expect(rows).toEqual([
      { layer: "l1", succeeded: 6, empty: 2, failed: 2, failureRate: 0.2 },
      { layer: "l2", succeeded: 0, empty: 0, failed: 0, failureRate: undefined },
      { layer: "l3", succeeded: 0, empty: 0, failed: 1, failureRate: 1 },
    ]);
  });

  it("works before the summary has loaded", () => {
    expect(layerRows(undefined).map((r) => r.failed)).toEqual([0, 0, 0]);
  });
});

const letter = (kind: DeadLetter["kind"], job: DeadLetter["job"]): DeadLetter => ({ kind, job, attempt: 3, dead_at_ms: 1 });

describe("dead letters", () => {
  it("names what each job was about", () => {
    expect(deadLetterSubject(letter("l1", { L1: { session_id: "s-1" } }))).toBe("session s-1");
    expect(deadLetterSubject(letter("skill", { SkillExtract: { task_id: "t-1" } }))).toBe("task t-1");
    expect(deadLetterSubject(letter("l2", { L2: { profile: { team_id: "team-1", agent_id: "agt-1" } } }))).toBe("agent agt-1");
    expect(deadLetterSubject(letter("l3", {}))).toBe("—");
  });

  it("counts per kind, every kind present", () => {
    const counts = countByKind([letter("l1", {}), letter("l1", {}), letter("skill", {})]);
    expect(counts).toEqual({ l1: 2, l2: 0, l3: 0, skill: 1 });
  });
});
