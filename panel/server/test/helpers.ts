import { vi } from "vitest";
import { UserKeyVerifier } from "../src/auth/user-key-verifier.js";
import { FetchCoreClient } from "../src/core-client/fetch-core-client.js";
import { createApp } from "../src/http/app.js";

export const CORE = "http://core.test:8420";

export type FetchMock = ReturnType<typeof vi.fn<typeof fetch>>;

export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

export function ok<T>(data: T): Record<string, unknown> {
  return { code: 0, message: "ok", request_id: "req-core", data };
}

/** Fetch mock that answers auth/verify as valid for `sk-mem-good` and delegates everything else. */
export function coreFetch(handler: (url: string, init: RequestInit) => Response | Promise<Response>): FetchMock {
  return vi.fn<typeof fetch>(async (input, init = {}) => {
    const url = String(input);
    if (url.endsWith("/v3/meta/auth/verify")) {
      const body = JSON.parse(String(init.body)) as { user_key: string };
      const users: Record<string, { user_id: string; user_type: string }> = {
        "sk-mem-good": { user_id: "usr_1", user_type: "system_admin" },
        "sk-mem-viewer": { user_id: "usr_2", user_type: "normal" },
      };
      const user = users[body.user_key] ?? null;
      return jsonResponse(ok({ valid: user !== null, user }));
    }
    return handler(url, init);
  });
}

export function buildApp(fetchFn: FetchMock, opts: { requireUserKey?: boolean; uiDistDir?: string; timeoutMs?: number } = {}) {
  const core = new FetchCoreClient({
    baseUrl: CORE,
    departmentId: "dept-1",
    timeoutMs: opts.timeoutMs ?? 1_000,
    fetchFn,
  });
  return createApp({
    core,
    verifier: opts.requireUserKey === false ? undefined : new UserKeyVerifier(core),
    publicConfig: { department_id: "dept-1", core_url_display: CORE },
    uiDistDir: opts.uiDistDir,
  });
}

export function post(path: string, body: unknown, headers: Record<string, string> = {}): RequestInit & { method: string } {
  return {
    method: "POST",
    headers: { "content-type": "application/json", "x-memory-user-key": "sk-mem-good", ...headers },
    body: typeof body === "string" ? body : JSON.stringify(body),
  };
}
