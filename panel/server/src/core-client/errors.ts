import type { CoreResponse } from "./port.js";

/** Core did not answer within the configured timeout. */
export class CoreTimeoutError extends Error {
  override readonly name = "CoreTimeoutError";
  constructor(readonly timeoutMs: number) {
    super(`Core did not respond within ${timeoutMs} ms`);
  }
}

/**
 * Core answered the key check with an error of its own, such as 429 for an
 * address locked out after too many refused keys, or 503. Relayed as is
 * (never read as "invalid key") and not cached.
 */
export class CoreRefusedError extends Error {
  override readonly name = "CoreRefusedError";
  constructor(readonly response: CoreResponse) {
    super(`Core answered ${response.status}`);
  }
}

/** Core could not be reached (connection refused, DNS failure, reset, ...). */
export class CoreUnavailableError extends Error {
  override readonly name = "CoreUnavailableError";
  constructor(cause: unknown) {
    super(cause instanceof Error ? cause.message : String(cause), { cause });
  }
}
