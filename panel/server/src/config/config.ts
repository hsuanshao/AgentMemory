/** Runtime configuration of the panel BFF, resolved once from the environment. */
export interface PanelConfig {
  readonly port: number;
  readonly host: string;
  readonly coreUrl: string;
  /**
   * `CORE_API_KEY` is set, though the panel no longer uses it: Core takes the
   * signed-in user's key on every route the panel calls, and the server API
   * key is for the operator only.
   */
  readonly staleCoreApiKey: boolean;
  readonly coreDepartmentId: string;
  readonly coreTimeoutMs: number;
  /** Directory holding the built SPA; `undefined` disables static serving. */
  readonly uiDistDir: string | undefined;
  /**
   * When true, every proxied call except auth/verify must carry a valid user
   * key. When false the panel forwards calls without checking, and Core
   * still refuses those without a key of the department.
   */
  readonly requireUserKey: boolean;
}

type Env = Readonly<Record<string, string | undefined>>;

const DEFAULTS = {
  port: 8125,
  host: "0.0.0.0",
  coreUrl: "http://127.0.0.1:8420",
  coreDepartmentId: "default",
  coreTimeoutMs: 15_000,
} as const;

export class ConfigError extends Error {
  override readonly name = "ConfigError";
}

export function loadConfig(env: Env = process.env): PanelConfig {
  return {
    port: parsePositiveInt(env.PORT, DEFAULTS.port, "PORT"),
    host: nonEmpty(env.HOST) ?? DEFAULTS.host,
    coreUrl: parseUrl(env.CORE_URL ?? DEFAULTS.coreUrl),
    staleCoreApiKey: nonEmpty(env.CORE_API_KEY) !== undefined,
    coreDepartmentId: nonEmpty(env.CORE_DEPARTMENT_ID) ?? DEFAULTS.coreDepartmentId,
    coreTimeoutMs: parsePositiveInt(env.CORE_TIMEOUT_MS, DEFAULTS.coreTimeoutMs, "CORE_TIMEOUT_MS"),
    uiDistDir: nonEmpty(env.UI_DIST_DIR),
    requireUserKey: parseBool(env.PANEL_REQUIRE_USER_KEY, true),
  };
}

/** Core URL without credentials, safe to show in the UI. */
export function displayCoreUrl(coreUrl: string): string {
  const url = new URL(coreUrl);
  url.username = "";
  url.password = "";
  return url.origin + url.pathname.replace(/\/$/, "");
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

function parsePositiveInt(raw: string | undefined, fallback: number, name: string): number {
  const value = nonEmpty(raw);
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new ConfigError(`${name} must be a positive integer, got "${value}"`);
  }
  return parsed;
}

function parseBool(raw: string | undefined, fallback: boolean): boolean {
  const value = nonEmpty(raw)?.toLowerCase();
  if (value === undefined) return fallback;
  return !["0", "false", "no", "off"].includes(value);
}

function parseUrl(raw: string): string {
  try {
    const url = new URL(raw.trim());
    return url.toString().replace(/\/$/, "");
  } catch {
    throw new ConfigError(`CORE_URL is not a valid URL: "${raw}"`);
  }
}
