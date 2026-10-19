import type { ButtonHTMLAttributes } from "react";
import { cx } from "./cx";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md";

const VARIANTS: Record<Variant, string> = {
  primary: "bg-accent text-white hover:bg-accent/90 border-transparent",
  secondary: "bg-surface text-fg border-line hover:bg-raised",
  ghost: "bg-transparent text-muted border-transparent hover:bg-raised hover:text-fg",
  danger: "bg-surface text-err border-line hover:bg-err/10 hover:border-err/40",
};

const SIZES: Record<Size, string> = {
  sm: "h-7 px-2 text-xs",
  md: "h-8 px-3 text-[13px]",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  busy?: boolean;
}

export function Button({ variant = "secondary", size = "md", busy, className, disabled, children, ...rest }: ButtonProps) {
  return (
    <button
      type="button"
      {...rest}
      disabled={disabled || busy}
      className={cx(
        "inline-flex items-center justify-center gap-1.5 whitespace-nowrap rounded-sm border font-medium transition-colors",
        "focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/50 disabled:cursor-not-allowed disabled:opacity-50",
        VARIANTS[variant],
        SIZES[size],
        className,
      )}
    >
      {busy && <span className="h-3 w-3 animate-spin rounded-full border-2 border-current border-t-transparent" />}
      {children}
    </button>
  );
}
