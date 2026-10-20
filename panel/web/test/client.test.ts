import { describe, expect, it, vi } from "vitest";
import { ApiError, errorMessage, PanelClient } from "../src/api/client";
import { createCoreApi } from "../src/api/core";

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function setup(response: Response | (() => Promise<Response>), userKey: string | undefined = "sk-mem-1") {
  const fetchFn = vi.fn<typeof fetch>(async () => (typeof response === "function" ? response() : response));
  const onUnauthorized = vi.fn();
  const client = new PanelClient({ getUserKey: () => userKey, onUnauthorized }, fetchFn);
  return { client, api: createCoreApi(client), fetchFn, onUnauthorized };
}

describe("PanelClient.core", () => {
  it("posts JSON with the user key and unwraps data", async () => {
    const { client, fetchFn } = setup(json({ code: 0, message: "ok", data: { total: 3 } }));
    await expect(client.core<{ total: number }>("atomic/count", { type: "persona" })).resolves.toEqual({ total: 3 });
    const [url, init] = fetchFn.mock.calls[0]!;
    expect(url).toBe("/api/core/atomic/count");
    expect(init?.method).toBe("POST");
    expect((init?.headers as Record<string, string>)["x-memory-user-key"]).toBe("sk-mem-1");
    expect(JSON.parse(String(init?.body))).toEqual({ type: "persona" });
  });

  it("throws ApiError on a non-zero code even with HTTP 200", async () => {
    const { client } = setup(json({ code: 40401, message: "SKILL_NOT_FOUND", request_id: "r1" }));
    const err = await client.core("skill/get", {}).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 200, code: 40401, message: "SKILL_NOT_FOUND", requestId: "r1" });
  });

  it("signs out on 401 from data routes", async () => {
    const { client, onUnauthorized } = setup(json({ code: 401, message: "unauthorized" }, 401));
    await expect(client.core("stats/overview")).rejects.toBeInstanceOf(ApiError);
    expect(onUnauthorized).toHaveBeenCalledOnce();
  });

  it("maps BFF error envelopes (504 CORE_TIMEOUT)", async () => {
    const { client } = setup(json({ code: 504, message: "CORE_TIMEOUT" }, 504));
    const err = await client.core("recall", {}).catch((e: unknown) => e);
    expect(errorMessage(err)).toBe("[504] CORE_TIMEOUT");
  });

  it("reports an unreachable BFF", async () => {
    const { client } = setup(() => Promise.reject(new TypeError("Failed to fetch")));
    await expect(client.core("recall", {})).rejects.toMatchObject({ status: 0, message: "Panel server unreachable" });
  });
});

describe("verifyUserKey", () => {
  it("returns valid=false without treating it as an error", async () => {
    const { api, onUnauthorized } = setup(json({ code: 0, message: "ok", data: { valid: false, user: null } }), undefined);
    await expect(api.verifyUserKey("sk-mem-bad")).resolves.toEqual({ valid: false, user: null });
    expect(onUnauthorized).not.toHaveBeenCalled();
  });
});

describe("distillation endpoints", () => {
  it("call the allowlisted paths with their filters", async () => {
    const { api, fetchFn } = setup(async () => json({ code: 0, message: "ok", data: { items: [], requeued: 2 } }));
    await api.generationLog.summary(6);
    await api.deadLetters.list({ kind: "l1", limit: 20 });
    await api.deadLetters.redrive({ kind: "skill", limit: 10 });
    const calls = fetchFn.mock.calls.map(([url, init]) => [url, JSON.parse(String(init?.body))]);
    expect(calls).toEqual([
      ["/api/core/memory-generation-log/summary", { hours: 6 }],
      ["/api/core/pipeline/dead-letter/list", { kind: "l1", limit: 20 }],
      ["/api/core/pipeline/dead-letter/redrive", { kind: "skill", limit: 10 }],
    ]);
  });
});
