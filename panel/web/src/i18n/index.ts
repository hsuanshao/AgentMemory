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

const translators = new Map<Locale, Translate>();

/** One translator per language, so its identity changes exactly when the language does. */
export function translatorFor(locale: Locale): Translate {
  let translate = translators.get(locale);
  if (!translate) {
    translate = (key, vars) => interpolate(CATALOGS[locale][key] ?? en[key] ?? key, vars);
    translators.set(locale, translate);
  }
  return translate;
}

/** The message in the current language, English when a catalog lacks it (for code outside components). */
export const t: Translate = (key, vars) => translatorFor(getLocale())(key, vars);

/**
 * The current language's translator, re-rendering the caller when the language
 * changes. A new language gives a new function, so `useMemo`/`React.memo` that
 * depend on it recompute.
 */
export function useT(): Translate {
  return translatorFor(useStore(localeStore));
}

/**
 * The current language. Keeps `<html lang>` in step: screen readers read in it,
 * hyphenation (long German words) follows it, and browsers pick the zh-TW or
 * zh-CN fallback font from it.
 */
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

