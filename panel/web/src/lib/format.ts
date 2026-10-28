/** Small presentation helpers shared by all features; numbers and relative times follow the panel language. */

import { getLocale, t } from "../i18n";

export function formatNumber(n: number | null | undefined): string {
  return typeof n === "number" && Number.isFinite(n) ? n.toLocaleString(getLocale()) : "—";
}

export function formatDuration(seconds: number | null | undefined): string {
  if (typeof seconds !== "number" || !Number.isFinite(seconds)) return "—";
  const s = Math.floor(seconds);
  const d = Math.floor(s / 86_400);
  const h = Math.floor((s % 86_400) / 3_600);
  const m = Math.floor((s % 3_600) / 60);
  if (d > 0) return `${unit(d, "day")} ${unit(h, "hour")}`;
  if (h > 0) return `${unit(h, "hour")} ${unit(m, "minute")}`;
  if (m > 0) return `${unit(m, "minute")} ${unit(s % 60, "second")}`;
  return unit(s, "second");
}

/** "3h" in English, "3小時" in Chinese. */
function unit(value: number, name: "day" | "hour" | "minute" | "second"): string {
  return new Intl.NumberFormat(getLocale(), { style: "unit", unit: name, unitDisplay: "narrow" }).format(value);
}

/** Accepts ISO strings, epoch ms, or epoch seconds. */
export function formatTime(value: string | number | null | undefined): string {
  if (value === null || value === undefined || value === "") return "—";
  const date = toDate(value);
  if (!date) return String(value);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(
    date.getMinutes(),
  )}:${pad(date.getSeconds())}`;
}

export function formatRelative(value: string | number | null | undefined, now: number = Date.now()): string {
  const date = value === null || value === undefined ? undefined : toDate(value);
  if (!date) return "—";
  const diff = Math.round((now - date.getTime()) / 1000);
  if (diff < 0) return t("time.justNow");
  const rtf = new Intl.RelativeTimeFormat(getLocale(), { style: "narrow" });
  if (diff < 60) return rtf.format(-diff, "second");
  if (diff < 3_600) return rtf.format(-Math.floor(diff / 60), "minute");
  if (diff < 86_400) return rtf.format(-Math.floor(diff / 3_600), "hour");
  return rtf.format(-Math.floor(diff / 86_400), "day");
}

export function truncate(text: string | null | undefined, max: number): string {
  if (!text) return "";
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

function toDate(value: string | number): Date | undefined {
  if (typeof value === "number") {
    const ms = value < 1e12 ? value * 1000 : value;
    return new Date(ms);
  }
  const numeric = Number(value);
  if (value.trim() !== "" && Number.isFinite(numeric)) return toDate(numeric);
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? undefined : parsed;
}
