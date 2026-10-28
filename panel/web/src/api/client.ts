import { t } from "../i18n";
import type { Envelope } from "./types";

/** Error raised for any non-success Core/BFF response. */
export class ApiError extends Error {
  override readonly name = "ApiError";
  constructor(
    readonly status: number,
    readonly code: number,
    message: string,
    readonly requestId?: string,
  ) {
    super(message);
  }

  get isUnauthorized(): boolean {
    return this.status === 401 || this.code === 401;
  }
}

export interface ClientHooks {
  /** Current user key, sent as `x-memory-user-key`. */
  getUserKey: () => string | undefined;
  /** Called when the BFF/Core rejects the user key. */
  onUnauthorized: () => void;
}

export interface CallOptions {
  signal?: AbortSignal;
}

type FetchFn = typeof fetch;

/** Typed client over the BFF (`/api/core/*`, `/api/core-health`, `/api/config`). */
export class PanelClient {
  constructor(
    private readonly hooks: ClientHooks,
    private readonly fetchFn: FetchFn = (...args) => fetch(...args),
    private readonly baseUrl = "",
  ) {}

  /** `POST /api/core/{path}` → unwrapped `data`. Throws `ApiError` when `code !== 0`. */
  async core<T>(path: string, body: object = {}, opts: CallOptions = {}): Promise<T> {
    const headers: Record<string, string> = { "content-type": "application/json" };
    const userKey = this.hooks.getUserKey();
    if (userKey) headers["x-memory-user-key"] = userKey;

    const res = await this.send(`/api/core/${path}`, { method: "POST", headers, body: JSON.stringify(body), signal: opts.signal });
    const envelope = await readEnvelope<T>(res);
    if (res.ok && envelope.code === 0) return envelope.data as T;

    const error = new ApiError(res.status, envelope.code, envelope.message || res.statusText, envelope.request_id);
    if (error.isUnauthorized && path !== "meta/auth/verify") this.hooks.onUnauthorized();
    throw error;
  }

  /** Plain GET returning raw JSON (used for `/api/config` and `/api/core-health`). */
  async getJson<T>(path: string, opts: CallOptions = {}): Promise<T> {
    const res = await this.send(path, { method: "GET", signal: opts.signal });
    const body = (await res.json().catch(() => null)) as (T & Partial<Envelope<unknown>>) | null;
    if (!res.ok || body === null) {
      throw new ApiError(res.status, body?.code ?? res.status, body?.message ?? res.statusText);
    }
    return body;
  }

  private async send(path: string, init: RequestInit): Promise<Response> {
    try {
      return await this.fetchFn(this.baseUrl + path, init);
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") throw err;
      throw new ApiError(0, 0, t("api.unreachable"));
    }
  }
}

async function readEnvelope<T>(res: Response): Promise<Envelope<T>> {
  try {
    const body = (await res.json()) as Partial<Envelope<T>>;
    return {
      code: typeof body.code === "number" ? body.code : res.status,
      message: typeof body.message === "string" ? body.message : "",
      request_id: body.request_id,
      data: body.data,
    };
  } catch {
    return { code: res.status, message: res.statusText || t("api.invalidResponse") };
  }
}

export function isAbort(err: unknown): boolean {
  return err instanceof DOMException && err.name === "AbortError";
}

export function errorMessage(err: unknown): string {
  if (err instanceof ApiError) {
    const status = err.code && err.code !== err.status ? `${err.status}/${err.code}` : String(err.status || t("api.network"));
    return `[${status}] ${err.message}`;
  }
  return err instanceof Error ? err.message : String(err);
}
