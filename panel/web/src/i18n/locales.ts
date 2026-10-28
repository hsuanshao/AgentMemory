/** Languages the panel ships, in the order the language menu lists them. */
export const LOCALES = [
  { code: "en", name: "English" },
  { code: "zh-TW", name: "繁體中文" },
  { code: "zh-CN", name: "简体中文" },
  { code: "fr", name: "Français" },
  { code: "vi", name: "Tiếng Việt" },
  { code: "es", name: "Español" },
  { code: "de", name: "Deutsch" },
] as const;

export type Locale = (typeof LOCALES)[number]["code"];

export const DEFAULT_LOCALE: Locale = "en";

export function isLocale(v: unknown): v is Locale {
  return LOCALES.some((l) => l.code === v);
}
