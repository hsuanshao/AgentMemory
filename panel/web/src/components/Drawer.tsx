import { useEffect, type ReactNode } from "react";
import { useT } from "../i18n";

export function Drawer({
  open,
  title,
  onClose,
  children,
  footer,
}: {
  open: boolean;
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
}) {
  const t = useT();
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;
  return (
    <div className="fixed inset-0 z-40 flex justify-end">
      <button type="button" aria-label={t("common.close")} className="absolute inset-0 bg-black/40" onClick={onClose} />
      <aside role="dialog" aria-modal className="relative flex h-full w-full max-w-2xl flex-col border-l border-line bg-surface shadow-xl">
        <header className="flex items-center justify-between gap-2 border-b border-line px-4 py-2.5">
          <h2 className="truncate text-sm font-semibold">{title}</h2>
          <button type="button" onClick={onClose} className="text-lg leading-none text-muted hover:text-fg" aria-label={t("common.close")}>
            ×
          </button>
        </header>
        <div className="flex-1 overflow-y-auto p-4">{children}</div>
        {footer && <footer className="flex justify-end gap-2 border-t border-line px-4 py-2.5">{footer}</footer>}
      </aside>
    </div>
  );
}
