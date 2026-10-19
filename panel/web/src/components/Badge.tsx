import type { ReactNode } from "react";
import { cx } from "./cx";

export type BadgeTone = "neutral" | "ok" | "warn" | "err" | "accent";

const TONES: Record<BadgeTone, string> = {
  neutral: "bg-raised text-muted border-line",
  ok: "bg-ok/10 text-ok border-ok/30",
  warn: "bg-warn/10 text-warn border-warn/30",
  err: "bg-err/10 text-err border-err/30",
  accent: "bg-accent/10 text-accent border-accent/30",
};

export function Badge({ tone = "neutral", children, className }: { tone?: BadgeTone; children: ReactNode; className?: string }) {
  return (
    <span className={cx("inline-flex items-center rounded-sm border px-1.5 py-px font-mono text-xxs", TONES[tone], className)}>
      {children}
    </span>
  );
}

export function StatusDot({ ok }: { ok: boolean | undefined }) {
  const color = ok === undefined ? "bg-muted" : ok ? "bg-ok" : "bg-err";
  return <span className={cx("inline-block h-2 w-2 rounded-full", color)} aria-hidden />;
}
