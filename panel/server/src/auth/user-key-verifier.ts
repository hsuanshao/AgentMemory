import { createHash } from "node:crypto";
import { CoreRefusedError } from "../core-client/errors.js";
import type { CoreClient } from "../core-client/port.js";

export interface VerifiedUser {
  readonly user_id: string;
  readonly user_type: string;
  readonly username?: string;
}

interface CacheEntry {
  readonly user: VerifiedUser | null;
  readonly expiresAt: number;
}

export interface UserKeyVerifierOptions {
  readonly ttlMs?: number;
  readonly maxEntries?: number;
  readonly now?: () => number;
}

/**
 * Verifies browser user keys against Core `meta/auth/verify`, caching the
 * verdict briefly so every proxied call does not cost an extra round trip.
 * The cache holds digests, not keys, and a revoked key stops working here
 * within `ttlMs` (Core refuses it at once). Core transport errors propagate
 * (they are not cached).
 */
export class UserKeyVerifier {
  private readonly cache = new Map<string, CacheEntry>();
  private readonly ttlMs: number;
  private readonly maxEntries: number;
  private readonly now: () => number;

  constructor(private readonly core: CoreClient, options: UserKeyVerifierOptions = {}) {
    this.ttlMs = options.ttlMs ?? 10_000;
    this.maxEntries = options.maxEntries ?? 1_000;
    this.now = options.now ?? Date.now;
  }

  async verify(userKey: string, call: { requestId: string; forwardedFor?: string }): Promise<VerifiedUser | null> {
    const digest = createHash("sha256").update(userKey).digest("hex");
    const cached = this.cache.get(digest);
    if (cached && cached.expiresAt > this.now()) return cached.user;

    const response = await this.core.callV3("meta/auth/verify", { user_key: userKey }, call);
    if (response.status !== 200) throw new CoreRefusedError(response);
    const user = extractUser(response.body);
    this.remember(digest, user);
    return user;
  }

  private remember(digest: string, user: VerifiedUser | null): void {
    if (this.cache.size >= this.maxEntries) {
      const oldest = this.cache.keys().next().value;
      if (oldest !== undefined) this.cache.delete(oldest);
    }
    this.cache.set(digest, { user, expiresAt: this.now() + this.ttlMs });
  }
}

function extractUser(body: unknown): VerifiedUser | null {
  if (!isRecord(body) || body.code !== 0 || !isRecord(body.data)) return null;
  const { valid, user } = body.data;
  if (valid !== true || !isRecord(user) || typeof user.user_id !== "string") return null;
  return {
    user_id: user.user_id,
    user_type: typeof user.user_type === "string" ? user.user_type : "normal",
    username: typeof user.username === "string" ? user.username : undefined,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
