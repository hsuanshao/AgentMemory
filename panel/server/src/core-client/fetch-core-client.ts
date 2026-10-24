import type { CoreCallContext, CoreClient, CoreResponse } from "./port.js";
import { CoreTimeoutError, CoreUnavailableError } from "./errors.js";

export interface FetchCoreClientOptions {
  readonly baseUrl: string;
  readonly departmentId: string;
  readonly timeoutMs: number;
  /** Injected for tests; defaults to the global `fetch`. */
  readonly fetchFn?: typeof fetch;
}

/** CoreClient adapter over WHATWG fetch with a hard timeout. */
export class FetchCoreClient implements CoreClient {
  private readonly fetchFn: typeof fetch;

  constructor(private readonly options: FetchCoreClientOptions) {
    this.fetchFn = options.fetchFn ?? globalThis.fetch.bind(globalThis);
  }

  callV3(path: string, body: unknown, ctx: CoreCallContext): Promise<CoreResponse> {
    const headers = this.baseHeaders(ctx);
    headers.set("content-type", "application/json");
    headers.set("x-memory-department-id", this.options.departmentId);
    if (ctx.userKey) headers.set("x-memory-user-key", ctx.userKey);
    return this.send(`/v3/${path}`, { method: "POST", headers, body: JSON.stringify(body ?? {}) });
  }

  health(ctx: CoreCallContext): Promise<CoreResponse> {
    return this.send("/health", { method: "GET", headers: this.baseHeaders(ctx) });
  }

  private baseHeaders(ctx: CoreCallContext): Headers {
    // No server API key: Core authorizes the user key of each call.
    const headers = new Headers({ accept: "application/json", "x-request-id": ctx.requestId });
    if (ctx.forwardedFor) headers.set("x-forwarded-for", ctx.forwardedFor);
    return headers;
  }

  private async send(path: string, init: RequestInit): Promise<CoreResponse> {
    const signal = AbortSignal.timeout(this.options.timeoutMs);
    let response: Response;
    try {
      response = await this.fetchFn(this.options.baseUrl + path, { ...init, signal });
    } catch (err) {
      throw this.mapFetchError(err);
    }
    try {
      const retryAfter = response.headers.get("retry-after") ?? undefined;
      return { status: response.status, body: await parseBody(response), ...(retryAfter ? { retryAfter } : {}) };
    } catch (err) {
      throw this.mapFetchError(err);
    }
  }

  private mapFetchError(err: unknown): Error {
    if (isTimeout(err)) return new CoreTimeoutError(this.options.timeoutMs);
    return new CoreUnavailableError(err);
  }
}

async function parseBody(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return { code: response.status, message: text.slice(0, 500) };
  }
}

function isTimeout(err: unknown): boolean {
  return err instanceof DOMException
    ? err.name === "TimeoutError" || err.name === "AbortError"
    : err instanceof Error && (err.name === "TimeoutError" || err.name === "AbortError");
}
