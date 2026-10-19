import type { InputHTMLAttributes, ReactNode, SelectHTMLAttributes, TextareaHTMLAttributes } from "react";
import { cx } from "./cx";

export function Field({ label, children, className }: { label: string; children: ReactNode; className?: string }) {
  return (
    <label className={cx("flex flex-col gap-1", className)}>
      <span className="label">{label}</span>
      {children}
    </label>
  );
}

export function Input({ className, ...rest }: InputHTMLAttributes<HTMLInputElement>) {
  return <input {...rest} className={cx("input", className)} />;
}

export interface Option<T extends string> {
  value: T;
  label: string;
}

export function Select<T extends string>({
  options,
  className,
  ...rest
}: SelectHTMLAttributes<HTMLSelectElement> & { options: ReadonlyArray<Option<T>> }) {
  return (
    <select {...rest} className={cx("input pr-6", className)}>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

export function TextArea({ className, ...rest }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      {...rest}
      className={cx(
        "w-full rounded-sm border border-line bg-surface p-2 font-mono text-[12px] leading-relaxed text-fg outline-hidden",
        "focus:border-accent focus:ring-1 focus:ring-accent/40",
        className,
      )}
    />
  );
}
