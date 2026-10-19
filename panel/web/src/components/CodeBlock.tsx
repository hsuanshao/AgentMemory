import type { ReactNode } from "react";
import { cx } from "./cx";

export function CodeBlock({ children, className }: { children: string; className?: string }) {
  return (
    <pre
      className={cx(
        "max-h-[60vh] overflow-auto whitespace-pre-wrap wrap-break-word rounded-sm border border-line bg-raised p-3 font-mono text-[12px] leading-relaxed",
        className,
      )}
    >
      {children}
    </pre>
  );
}

export function JsonView({ value, className }: { value: unknown; className?: string }) {
  return <CodeBlock className={className}>{JSON.stringify(value, null, 2)}</CodeBlock>;
}

/** Key/value grid for detail panels. */
export function KeyValues({ entries }: { entries: ReadonlyArray<[string, ReactNode]> }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1">
      {entries.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="label pt-0.5">{k}</dt>
          <dd className="mono break-all">{v ?? "—"}</dd>
        </div>
      ))}
    </dl>
  );
}
