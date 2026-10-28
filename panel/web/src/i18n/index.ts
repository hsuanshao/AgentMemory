import { useEffect } from "react";
import { useStore } from "../hooks/useStore";
import { createPersistedStore } from "../lib/store";
import { isLocale, type Locale } from "./locales";
import { matchLocale } from "./match";
import { de } from "./messages/de";
import { en, type MessageKey, type Messages } from "./messages/en";
import { es } from "./messages/es";
import { fr } from "./messages/fr";
import { vi } from "./messages/vi";
import { zhCN } from "./messages/zh-CN";
import { zhTW } from "./messages/zh-TW";

export { DEFAULT_LOCALE, isLocale, LOCALES, type Locale } from "./locales";
export type { MessageKey } from "./messages/en";

export const CATALOGS: Record<Locale, Messages> = { en, "zh-TW": zhTW, "zh-CN": zhCN, fr, vi, es, de };

export type Vars = Record<string, string | number>;
export type Translate = (key: MessageKey, vars?: Vars) => string;

function browserLanguages(): readonly string[] {
  if (typeof navigator === "undefined") return [];
  return navigator.languages?.length ? navigator.languages : navigator.language ? [navigator.language] : [];
}

/** The viewer's pick, remembered per browser; the browser's languages until then. */
export const localeStore = createPersistedStore<Locale>("memory-panel.locale", matchLocale(browserLanguages()), isLocale);

export function getLocale(): Locale {
  return localeStore.get();
}

/** Replaces each `{name}` with `vars.name`; unknown names are left as they are. */
export function interpolate(template: string, vars?: Vars): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (whole, name: string) => (name in vars ? String(vars[name]) : whole));
}

/** The message in the current language, English when a catalog lacks it. */
export const t: Translate = (key, vars) => interpolate(CATALOGS[getLocale()][key] ?? en[key] ?? key, vars);

/** `t`, re-rendering the caller when the language changes. */
export function useT(): Translate {
  useStore(localeStore);
  return t;
}

/** The current language; keeps `<html lang>` in step for screen readers and fonts. */
export function useLocale(): Locale {
  const locale = useStore(localeStore);
  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);
  return locale;
}

/** "Teams (3)", or just "Teams" until the count is known. */
export function withCount(label: string, count: number | undefined): string {
  return count === undefined ? label : t("common.withCount", { label, count });
}

