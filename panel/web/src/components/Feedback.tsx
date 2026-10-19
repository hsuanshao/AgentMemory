import type { ReactNode } from "react";
import { errorMessage } from "../api";
import { cx } from "./cx";

export function ErrorBanner({ error, onRetry, className }: { error: unknown; onRetry?: () => void; className?: string }) {
  if (!error) return null;
  return (
    <div className={cx("flex items-start justify-between gap-3 rounded-sm border border-err/40 bg-err/10 px-3 py-2 text-err", className)}>
      <span className="mono break-all">{errorMessage(error)}</span>
      {onRetry && (
        <button type="button" onClick={onRetry} className="shrink-0 text-xs font-medium underline">
          Retry
        </button>
      )}
    </div>
  );
}

export function Spinner({ className }: { className?: string }) {
  return (
    <span
      role="status"
      aria-label="Loading"
      className={cx("inline-block h-4 w-4 animate-spin rounded-full border-2 border-muted border-t-transparent", className)}
    />
  );
}

export function EmptyState({ children }: { children: ReactNode }) {
  return <div className="py-8 text-center text-muted">{children}</div>;
}

export function Notice({ children, tone = "info" }: { children: ReactNode; tone?: "info" | "ok" }) {
  return (
    <div
      className={cx(
        "rounded-sm border px-3 py-2 text-xs",
        tone === "ok" ? "border-ok/40 bg-ok/10 text-ok" : "border-accent/30 bg-accent/5 text-fg",
      )}
    >
      {children}
    </div>
  );
}
