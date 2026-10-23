/** Web Storage wrapper that never throws (private mode, blocked storage, SSR). */

/** `local` outlives the tab; `session` ends with it (used for credentials). */
export type StorageArea = "local" | "session";

function store(area: StorageArea): Storage | undefined {
  try {
    if (area === "session") return typeof sessionStorage === "undefined" ? undefined : sessionStorage;
    return typeof localStorage === "undefined" ? undefined : localStorage;
  } catch {
    return undefined;
  }
}

export function readJson<T>(key: string, isValid: (v: unknown) => v is T, area: StorageArea = "local"): T | undefined {
  try {
    const raw = store(area)?.getItem(key);
    if (!raw) return undefined;
    const parsed: unknown = JSON.parse(raw);
    return isValid(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

export function writeJson(key: string, value: unknown, area: StorageArea = "local"): void {
  try {
    store(area)?.setItem(key, JSON.stringify(value));
  } catch {
    /* storage unavailable: the value lives in memory only */
  }
}

export function remove(key: string, area: StorageArea = "local"): void {
  try {
    store(area)?.removeItem(key);
  } catch {
    /* ignore */
  }
}
