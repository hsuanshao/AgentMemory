import { NavLink, Outlet } from "react-router-dom";
import { api } from "../api";
import { signOut, sessionStore } from "../features/auth/session";
import { useAsync } from "../hooks/useAsync";
import { useStore } from "../hooks/useStore";
import { useT, type MessageKey } from "../i18n";
import { Badge } from "./Badge";
import { cx } from "./cx";
import { LanguageSelect } from "./LanguageSelect";

const NAV: ReadonlyArray<{ to: string; label: MessageKey; icon: string }> = [
  { to: "/dashboard", label: "nav.dashboard", icon: "▦" },
  { to: "/memory", label: "nav.memory", icon: "◫" },
  { to: "/logs", label: "nav.logs", icon: "≡" },
  { to: "/skills", label: "nav.skills", icon: "✦" },
  { to: "/meta", label: "nav.meta", icon: "◉" },
];

export function Layout() {
  const t = useT();
  const session = useStore(sessionStore);
  const config = useAsync((signal) => api.panelConfig({ signal }), []);

  return (
    <div className="flex h-full">
      <aside className="flex w-52 shrink-0 flex-col border-r border-line bg-surface">
        <div className="border-b border-line px-4 py-3">
          <div className="flex items-center gap-2 text-sm font-semibold">
            <img src="/favicon.svg" alt="" className="h-5 w-5" />
            Memory Panel
          </div>
          <div className="mono mt-0.5 truncate text-muted" title={config.data?.core_url_display}>
            {config.data ? t("layout.department", { id: config.data.department_id }) : "…"}
          </div>
        </div>
        <nav className="flex-1 space-y-0.5 p-2">
          {NAV.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              className={({ isActive }) =>
                cx(
                  "flex items-center gap-2 rounded-sm px-2 py-1.5 text-[13px]",
                  isActive ? "bg-accent/10 font-medium text-accent" : "text-muted hover:bg-raised hover:text-fg",
                )
              }
            >
              <span className="w-4 text-center" aria-hidden>
                {item.icon}
              </span>
              {t(item.label)}
            </NavLink>
          ))}
        </nav>
        <div className="border-t border-line px-3 pt-3">
          <LanguageSelect className="w-full" />
        </div>
        {session && (
          <div className="p-3">
            <div className="flex items-center justify-between gap-2">
              <div className="min-w-0">
                <div className="truncate text-xs font-medium">{session.user.username || session.user.user_id}</div>
                <div className="mono truncate text-muted">{session.user.user_id}</div>
              </div>
              {session.user.user_type === "system_admin" && <Badge tone="accent">admin</Badge>}
            </div>
            <button type="button" onClick={signOut} className="mt-2 text-xs text-muted hover:text-err">
              {t("layout.signOut")}
            </button>
          </div>
        )}
      </aside>
      <main className="min-w-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-[1400px] p-5">
          <Outlet />
        </div>
      </main>
    </div>
  );
}
