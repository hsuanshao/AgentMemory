import { describe, expect, it } from "vitest";
import { fromLink, memoryLink } from "../src/features/memory/scope";

const params = (link: string) => new URLSearchParams(link.split("?")[1]);

describe("memory links", () => {
  it("round-trip a user's scope and tab", () => {
    const link = memoryLink({ team_id: "team-1", agent_id: "agt-1", user_id: "usr-1" }, "l1");
    expect(link).toBe("/memory?team=team-1&agent=agt-1&user=usr-1&tab=l1");
    expect(fromLink(params(link))).toEqual({ scope: { team_id: "team-1", agent_id: "agt-1", user_id: "usr-1" }, tab: "l1" });
  });

  it("leave the user empty for an agent's shared layers", () => {
    const link = memoryLink({ team_id: "team-1", agent_id: "agt-1", user_id: "" }, "l2");
    expect(fromLink(params(link))?.scope.user_id).toBe("");
  });

  it("ignore links without a team and an agent, and unknown tabs", () => {
    expect(fromLink(new URLSearchParams("team=team-1"))).toBeUndefined();
    expect(fromLink(new URLSearchParams("team=t&agent=a&tab=admin"))?.tab).toBeUndefined();
  });
});
