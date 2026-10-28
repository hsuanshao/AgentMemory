import type { ReactNode } from "react";
import { cx } from "./cx";

export function Card({
  title,
  actions,
  children,
  className,
  bodyClassName,
}: {
  title?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
  bodyClassName?: string;
}) {
  return (
    <section className={cx("rounded-md border border-line bg-surface", className)}>
      {(title || actions) && (
        <header className="flex items-center justify-between gap-2 border-b border-line px-3 py-2">
          <h2 className="text-xs font-semibold uppercase tracking-wide text-muted">{title}</h2>
          {actions && <div className="flex items-center gap-2">{actions}</div>}
        </header>
      )}
      <div className={cx("p-3", bodyClassName)}>{children}</div>
    </section>
  );
}

type Tone = "default" | "ok" | "warn" | "err" | "accent";

const TONES: Record<Tone, string> = {
  default: "text-fg",
  ok: "text-ok",
  warn: "text-warn",
  err: "text-err",
  accent: "text-accent",
};

export function Stat({ label, value, hint, tone = "default" }: { label: string; value: ReactNode; hint?: ReactNode; tone?: Tone }) {
  return (
    <div className="rounded-md border border-line bg-surface px-3 py-2.5">
      {/* Long words (German) break by the page language instead of overflowing. */}
      <div className="label hyphens-auto wrap-break-word">{label}</div>
      <div className={cx("mt-1 font-mono text-xl font-semibold tabular-nums", TONES[tone])}>{value}</div>
      {hint && <div className="mt-0.5 truncate text-xxs text-muted">{hint}</div>}
    </div>
  );
}
