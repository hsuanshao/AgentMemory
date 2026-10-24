import { getConnInfo } from "@hono/node-server/conninfo";
import { Hono, type Context } from "hono";
import type { ContentfulStatusCode } from "hono/utils/http-status";
import type { UserKeyVerifier } from "../../auth/user-key-verifier.js";
import { CoreRefusedError } from "../../core-client/errors.js";
import type { CoreClient, CoreResponse } from "../../core-client/port.js";
import type { AppEnv } from "../app-env.js";
import { ADMIN_ONLY_V3_PATHS, PUBLIC_V3_PATHS, resolveV3Path } from "../allowlist.js";
import { errorEnvelope, PanelErrors } from "../envelope.js";

export interface CoreProxyDeps {
  readonly core: CoreClient;
  /** When set, non-public paths require a valid `x-memory-user-key`. */
  readonly verifier: UserKeyVerifier | undefined;
}

const PREFIX = "/api/core/";

/** `POST /api/core/*` → Core `POST /v3/*` (allowlisted) and `GET /api/core-health`. */
export function coreProxyRoutes(deps: CoreProxyDeps): Hono<AppEnv> {
  const app = new Hono<AppEnv>();

  app.get("/api/core-health", async (c) => {
    const response = await deps.core.health({ requestId: c.get("requestId") });
    return relay(c, response);
  });

  app.post(`${PREFIX}*`, async (c) => {
    const requestId = c.get("requestId");
    const path = resolveV3Path(c.req.path.slice(PREFIX.length));
    if (!path) return c.json(errorEnvelope(404, PanelErrors.NOT_FOUND, requestId), 404);

    const body = await readJsonBody(c);
    if (body === INVALID_JSON) return c.json(errorEnvelope(400, PanelErrors.BAD_REQUEST, requestId), 400);

    const userKey = c.req.header("x-memory-user-key")?.trim() || undefined;
    const call = { requestId, forwardedFor: forwardedFor(c) };
    let denied: Awaited<ReturnType<typeof checkUserKey>>;
    try {
      denied = await checkUserKey(deps.verifier, path, userKey, call);
    } catch (err) {
      if (err instanceof CoreRefusedError) return relay(c, err.response);
      throw err;
    }
    if (denied) return c.json(errorEnvelope(denied.status, denied.message, requestId), denied.status);

    const response = await deps.core.callV3(path, body, { ...call, userKey });
    return relay(c, response);
  });

  return app;
}

/**
 * The chain Core sees: the `X-Forwarded-For` this request arrived with (a
 * TLS proxy in front of the Panel) and the address that connected here.
 * A browser can write the header too; Core reads the chain from the right
 * and stops at the first address it does not trust, so that is harmless.
 */
function forwardedFor(c: Context<AppEnv>): string | undefined {
  let peer: string | undefined;
  try {
    peer = getConnInfo(c).remote.address;
  } catch {
    peer = undefined; // no socket (tests calling `app.request`)
  }
  const chain = [c.req.header("x-forwarded-for")?.trim(), peer].filter((p): p is string => Boolean(p));
  return chain.length > 0 ? chain.join(", ") : undefined;
}

async function checkUserKey(
  verifier: UserKeyVerifier | undefined,
  path: string,
  userKey: string | undefined,
  call: { requestId: string; forwardedFor?: string },
): Promise<{ status: 401 | 403; message: string } | undefined> {
  if (!verifier || PUBLIC_V3_PATHS.has(path)) return undefined;
  if (!userKey) return { status: 401, message: PanelErrors.MISSING_USER_KEY };
  const user = await verifier.verify(userKey, call);
  if (!user) return { status: 401, message: PanelErrors.UNAUTHORIZED };
  if (ADMIN_ONLY_V3_PATHS.has(path) && user.user_type !== "system_admin") {
    return { status: 403, message: PanelErrors.ADMIN_ONLY };
  }
  return undefined;
}

const INVALID_JSON = Symbol("invalid-json");

async function readJsonBody(c: Context<AppEnv>): Promise<unknown> {
  const text = await c.req.text();
  if (!text.trim()) return {};
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return INVALID_JSON;
  }
}

function relay(c: Context<AppEnv>, response: CoreResponse): Response {
  const status = toContentfulStatus(response.status);
  if (response.retryAfter) c.header("retry-after", response.retryAfter);
  return c.json(response.body ?? errorEnvelope(status, "EMPTY_CORE_RESPONSE", c.get("requestId")), status);
}

function toContentfulStatus(status: number): ContentfulStatusCode {
  // 1xx/204/304 cannot carry a body; Core never sends them for JSON routes.
  if (status < 200 || status === 204 || status === 205 || status === 304 || status > 599) return 502;
  return status as ContentfulStatusCode;
}
