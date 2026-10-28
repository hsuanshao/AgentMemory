import type { ReactNode } from "react";
import { cx } from "./cx";
import { useT } from "../i18n";
import { EmptyState, Spinner } from "./Feedback";

export interface Column<T> {
  key: string;
  header: ReactNode;
  render: (row: T) => ReactNode;
  className?: string;
}

export interface DataTableProps<T> {
  columns: ReadonlyArray<Column<T>>;
  rows: ReadonlyArray<T> | undefined;
  rowKey: (row: T) => string;
  loading?: boolean;
  empty?: ReactNode;
  onRowClick?: (row: T) => void;
  selectedKey?: string;
}

export function DataTable<T>({ columns, rows, rowKey, loading, empty, onRowClick, selectedKey }: DataTableProps<T>) {
  const t = useT();
  return (
    <div className="relative overflow-x-auto">
      <table className="w-full border-collapse text-left">
        <thead>
          <tr className="border-b border-line">
            {columns.map((c) => (
              <th key={c.key} className={cx("whitespace-nowrap px-2 py-1.5 label", c.className)}>
                {c.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows?.map((row) => {
            const key = rowKey(row);
            return (
              <tr
                key={key}
                onClick={onRowClick ? () => onRowClick(row) : undefined}
                className={cx(
                  "border-b border-line/60 align-top last:border-0",
                  onRowClick && "cursor-pointer hover:bg-raised",
                  selectedKey === key && "bg-accent/5",
                )}
              >
                {columns.map((c) => (
                  <td key={c.key} className={cx("px-2 py-1.5", c.className)}>
                    {c.render(row)}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
      {!loading && rows?.length === 0 && <EmptyState>{empty ?? t("common.noData")}</EmptyState>}
      {loading && !rows && (
        <div className="flex justify-center py-6">
          <Spinner />
        </div>
      )}
    </div>
  );
}
