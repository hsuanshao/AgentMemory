import { NavLink, Outlet } from "react-router-dom";
import { api } from "../api";
import { signOut, sessionStore } from "../features/auth/session";
import { useAsync } from "../hooks/useAsync";
import { useStore } from "../hooks/useStore";
import { Badge } from "./Badge";
import { cx } from "./cx";

const NAV = [
  { to: "/dashboard", label: "Dashboard", icon: "▦" },
  { to: "/memory", label: "Memory", icon: "◫" },
  { to: "/logs", label: "Generation logs", icon: "≡" },
  { to: "/skills", label: "Skills", icon: "✦" },
  { to: "/meta", label: "Metadata", icon: "◉" },
] as const;

export function Layout() {
  const session = useStore(sessionStore);
  const config = useAsync((signal) => api.panelConfig({ signal }), []);

  return (
    <div className="flex h-full">
      <aside className="flex w-52 shrink-0 flex-col border-r border-line bg-surface">
        <div className="border-b border-line px-4 py-3">
          <div className="text-sm font-semibold">Memory Panel</div>
          <div className="mono mt-0.5 truncate text-muted" title={config.data?.core_url_display}>
            {config.data ? `instance: ${config.data.instance_id}` : "…"}
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
              {item.label}
            </NavLink>
          ))}
        </nav>
        {session && (
          <div className="border-t border-line p-3">
            <div className="flex items-center justify-between gap-2">
              <div className="min-w-0">
                <div className="truncate text-xs font-medium">{session.user.username || session.user.user_id}</div>
                <div className="mono truncate text-muted">{session.user.user_id}</div>
              </div>
              {session.user.user_type === "system_admin" && <Badge tone="accent">admin</Badge>}
            </div>
            <button type="button" onClick={signOut} className="mt-2 text-xs text-muted hover:text-err">
              Sign out
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
