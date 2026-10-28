import { useEffect, useState, type FormEvent } from "react";
import { useT } from "../i18n";
import { Button } from "./Button";
import { Input } from "./Field";

/** Submit-on-enter search input with a clear button. Calls `onSearch("")` to clear. */
export function SearchBox({ value, onSearch, placeholder }: { value: string; onSearch: (q: string) => void; placeholder?: string }) {
  const t = useT();
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);

  function submit(e: FormEvent) {
    e.preventDefault();
    onSearch(draft.trim());
  }

  return (
    <form onSubmit={submit} className="flex gap-1">
      <Input value={draft} onChange={(e) => setDraft(e.target.value)} placeholder={placeholder ?? t("common.searchPlaceholder")} className="w-64" />
      <Button type="submit" size="md">
        {t("common.search")}
      </Button>
      {value && (
        <Button variant="ghost" onClick={() => onSearch("")}>
          {t("common.clear")}
        </Button>
      )}
    </form>
  );
}
