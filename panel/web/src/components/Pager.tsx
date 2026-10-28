import { useT } from "../i18n";
import { Button } from "./Button";

export function Pager({
  offset,
  limit,
  total,
  count,
  onChange,
}: {
  offset: number;
  limit: number;
  /** Total rows if the endpoint reports it. */
  total?: number;
  /** Rows on the current page (used when total is unknown). */
  count: number;
  onChange: (offset: number) => void;
}) {
  const t = useT();
  const hasNext = total !== undefined ? offset + limit < total : count >= limit;
  const end = offset + count;
  return (
    <div className="flex items-center justify-between gap-2 px-2 py-2 text-xs text-muted">
      <span className="mono">
        {total !== undefined
          ? t("pager.rangeOf", { from: count === 0 ? 0 : offset + 1, to: end, total })
          : t("pager.range", { from: count === 0 ? 0 : offset + 1, to: end })}
      </span>
      <div className="flex gap-1">
        <Button size="sm" disabled={offset === 0} onClick={() => onChange(Math.max(0, offset - limit))}>
          {t("pager.prev")}
        </Button>
        <Button size="sm" disabled={!hasNext} onClick={() => onChange(offset + limit)}>
          {t("pager.next")}
        </Button>
      </div>
    </div>
  );
}
