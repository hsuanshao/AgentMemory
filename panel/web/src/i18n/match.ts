import { DEFAULT_LOCALE, isLocale, type Locale } from "./locales";

/** Chinese tags written in simplified characters; every other Chinese tag, `zh` included, is traditional. */
const SIMPLIFIED = /^zh-(cn|sg|hans)(-|$)/;

/**
 * The panel language for a browser's preferred languages (`navigator.languages`,
 * most preferred first), used until the viewer picks one from the menu.
 *
 * Browsers send BCP 47 tags such as "zh-TW", "zh-Hant-HK", "zh-Hans", "zh",
 * "fr-CA", "pt-BR" or "de-AT". Regional variants fall back to their language.
 */
export function matchLocale(preferred: readonly string[]): Locale {
  for (const raw of preferred) {
    const tag = raw.toLowerCase();
    const base = tag.split("-")[0];
    if (base === "zh") return SIMPLIFIED.test(tag) ? "zh-CN" : "zh-TW";
    if (isLocale(base)) return base;
  }
  return DEFAULT_LOCALE;
}
