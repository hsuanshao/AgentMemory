import { isLocale, LOCALES, localeStore, useLocale, useT } from "../i18n";
import { Select } from "./Field";

const OPTIONS = LOCALES.map((l) => ({ value: l.code, label: l.name }));

/** Language menu; each language is listed in its own name. */
export function LanguageSelect({ className }: { className?: string }) {
  const t = useT();
  const locale = useLocale();
  return (
    <Select
      aria-label={t("layout.language")}
      title={t("layout.language")}
      options={OPTIONS}
      value={locale}
      onChange={(e) => {
        const next = e.target.value;
        if (isLocale(next)) localeStore.set(next);
      }}
      className={className}
    />
  );
}
