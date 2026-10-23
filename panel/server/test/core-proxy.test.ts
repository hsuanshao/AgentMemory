import { describe, expect, it } from "vitest";
import { buildApp, CORE, coreFetch, jsonResponse, ok, post } from "./helpers.js";

describe("POST /api/core/*", () => {
  it("forwards allowlisted calls with Core auth headers", async () => {
    const fetchFn = coreFetch(() => jsonResponse(ok({ items: [], total: 0 })));
    const app = buildApp(fetchFn);

    const res = await app.request("/api/core/atomic/query", post("", { limit: 5 }, { "x-request-id": "rid-1" }));

    expect(res.status).toBe(200);
    expect(await res.json()).toEqual(ok({ items: [], total: 0 }));
    const call = fetchFn.mock.calls.find(([url]) => String(url).endsWith("/v3/atomic/query"));
    expect(call).toBeDefined();
    const [url, init] = call!;
    expect(String(url)).toBe(`${CORE}/v3/atomic/query`);
    const headers = new Headers(init?.headers);
    expect(headers.get("authorization")).toBe("Bearer secret-core-key");
    expect(headers.get("x-memory-department-id")).toBe("dept-1");
    expect(headers.get("x-memory-user-key")).toBe("sk-mem-good");
    expect(headers.get("x-request-id")).toBe("rid-1");
    expect(JSON.parse(String(init?.body))).toEqual({ limit: 5 });
    expect(res.headers.get("x-request-id")).toBe("rid-1");
  });

  it("relays Core error statuses and envelopes unchanged", async () => {
    const envelope = { code: 404, message: "Store not available", request_id: "r" };
    const app = buildApp(coreFetch(() => jsonResponse(envelope, 404)));
    const res = await app.request("/api/core/scenario/read", post("", { path: "a.md" }));
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual(envelope);
  });

  it("returns a 404 envelope for non-allowlisted paths without calling Core", async () => {
    const fetchFn = coreFetch(() => jsonResponse(ok({})));
    const app = buildApp(fetchFn);
    const res = await app.request("/api/core/department/destroy", post("", { department_id: "x" }));
    expect(res.status).toBe(404);
    expect(await res.json()).toMatchObject({ code: 404, message: "NOT_FOUND" });
    expect(fetchFn).not.toHaveBeenCalled();
  });

  it("maps a Core timeout to 504 CORE_TIMEOUT", async () => {
    const fetchFn = coreFetch(() => Promise.reject(new DOMException("timed out", "TimeoutError")));
    const res = await buildApp(fetchFn).request("/api/core/stats/overview", post("", {}));
    expect(res.status).toBe(504);
    expect(await res.json()).toMatchObject({ code: 504, message: "CORE_TIMEOUT" });
  });

  it("enforces the timeout against a hanging Core", async () => {
    const fetchFn = coreFetch(
      (_url, init) =>
        new Promise<Response>((_resolve, reject) => {
          init.signal?.addEventListener("abort", () => reject(init.signal?.reason));
        }),
    );
    const res = await buildApp(fetchFn, { timeoutMs: 20 }).request("/api/core/pipeline/status", post("", {}));
    expect(res.status).toBe(504);
  });

  it("maps an unreachable Core to 502 CORE_UNAVAILABLE", async () => {
    const fetchFn = coreFetch(() => Promise.reject(new TypeError("fetch failed")));
    const res = await buildApp(fetchFn).request("/api/core/stats/overview", post("", {}));
    expect(res.status).toBe(502);
    expect(await res.json()).toMatchObject({ code: 502, message: "CORE_UNAVAILABLE" });
  });

  it("rejects malformed JSON with 400", async () => {
    const res = await buildApp(coreFetch(() => jsonResponse(ok({})))).request("/api/core/recall", post("", "{oops"));
    expect(res.status).toBe(400);
  });

  it("wraps non-JSON Core bodies in an envelope", async () => {
    const app = buildApp(coreFetch(() => new Response("upstream exploded", { status: 500 })));
    const res = await app.request("/api/core/recall", post("", { query: "x" }));
    expect(res.status).toBe(500);
    expect(await res.json()).toMatchObject({ code: 500, message: "upstream exploded" });
  });
});

describe("user key guard", () => {
  it("lets auth/verify through without a user key", async () => {
    const fetchFn = coreFetch(() => jsonResponse(ok({})));
    const res = await buildApp(fetchFn).request("/api/core/meta/auth/verify", {
      method: "POST",
      body: JSON.stringify({ user_key: "sk-mem-bad" }),
    });
    expect(res.status).toBe(200);
    expect(await res.json()).toMatchObject({ data: { valid: false } });
  });

  it("rejects calls without a user key", async () => {
    const fetchFn = coreFetch(() => jsonResponse(ok({})));
    const res = await buildApp(fetchFn).request("/api/core/stats/overview", { method: "POST", body: "{}" });
    expect(res.status).toBe(401);
    expect(fetchFn).not.toHaveBeenCalled();
  });

  it("rejects calls with an invalid user key", async () => {
    const fetchFn = coreFetch(() => jsonResponse(ok({})));
    const res = await buildApp(fetchFn).request("/api/core/stats/overview", post("", {}, { "x-memory-user-key": "sk-mem-bad" }));
    expect(res.status).toBe(401);
    expect(await res.json()).toMatchObject({ message: "unauthorized: invalid_user_key" });
  });

  it("caches a successful verification", async () => {
    const fetchFn = coreFetch(() => jsonResponse(ok({})));
    const app = buildApp(fetchFn);
    await app.request("/api/core/stats/overview", post("", {}));
    await app.request("/api/core/stats/overview", post("", {}));
    const verifies = fetchFn.mock.calls.filter(([url]) => String(url).endsWith("/meta/auth/verify"));
    expect(verifies).toHaveLength(1);
  });

  it("keeps writes and administration to system admins", async () => {
    const fetchFn = coreFetch(() => jsonResponse(ok({})));
    const app = buildApp(fetchFn);
    const viewer = { "x-memory-user-key": "sk-mem-viewer" };
    for (const path of [
      "meta/team/create",
      "meta/team-member/add",
      "atomic/delete",
      "core/write",
      "pipeline/dead-letter/redrive",
      "meta/redaction-rule/list",
      "meta/redaction-rule/create",
      "meta/redaction-rule/test",
    ]) {
      const res = await app.request(`/api/core/${path}`, post("", {}, viewer));
      expect(res.status, path).toBe(403);
      expect(await res.json()).toMatchObject({ message: "forbidden: system_admin_required" });
    }
    expect(fetchFn.mock.calls.filter(([url]) => !String(url).endsWith("/meta/auth/verify"))).toHaveLength(0);

    const read = await app.request("/api/core/meta/team/list", post("", {}, viewer));
    expect(read.status).toBe(200);
    const deadLetters = await app.request("/api/core/pipeline/dead-letter/list", post("", {}, viewer));
    expect(deadLetters.status, "anyone may look at dead letters").toBe(200);
    const admin = await app.request("/api/core/meta/team/create", post("", { name: "t" }));
    expect(admin.status).toBe(200);
  });

  it("can be disabled", async () => {
    const fetchFn = coreFetch(() => jsonResponse(ok({})));
    const res = await buildApp(fetchFn, { requireUserKey: false }).request("/api/core/stats/overview", {
      method: "POST",
      body: "{}",
    });
    expect(res.status).toBe(200);
  });
});

describe("GET /api/core-health", () => {
  it("proxies Core /health with the Bearer key", async () => {
    const fetchFn = coreFetch(() => jsonResponse({ status: "ok", version: "0.1.0", uptime: 12 }));
    const res = await buildApp(fetchFn).request("/api/core-health");
    expect(res.status).toBe(200);
    expect(await res.json()).toMatchObject({ status: "ok", uptime: 12 });
    const [url, init] = fetchFn.mock.calls[0]!;
    expect(String(url)).toBe(`${CORE}/health`);
    expect(init?.method).toBe("GET");
    expect(new Headers(init?.headers).get("authorization")).toBe("Bearer secret-core-key");
  });

  it("maps an unreachable Core to 502", async () => {
    const fetchFn = coreFetch(() => Promise.reject(new TypeError("ECONNREFUSED")));
    const res = await buildApp(fetchFn).request("/api/core-health");
    expect(res.status).toBe(502);
  });
});
