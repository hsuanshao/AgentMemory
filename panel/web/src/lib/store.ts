import { readJson, remove, writeJson, type StorageArea } from "./storage";

export interface Store<T> {
  get(): T;
  set(next: T): void;
  subscribe(listener: () => void): () => void;
}

/** Minimal observable value persisted to Web Storage (best effort). */
export function createPersistedStore<T>(
  key: string,
  initial: T,
  isValid: (v: unknown) => v is T,
  area: StorageArea = "local",
): Store<T> {
  let value: T = readJson(key, isValid, area) ?? initial;
  const listeners = new Set<() => void>();

  return {
    get: () => value,
    set(next) {
      value = next;
      if (next === null || next === undefined) remove(key, area);
      else writeJson(key, next, area);
      listeners.forEach((l) => l());
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
