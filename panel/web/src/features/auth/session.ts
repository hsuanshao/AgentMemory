import type { UserPublic } from "../../api/types";
import { useStore } from "../../hooks/useStore";
import { remove } from "../../lib/storage";
import { createPersistedStore } from "../../lib/store";

export interface Session {
  userKey: string;
  user: UserPublic;
  verifiedAt: number;
}

function isSession(v: unknown): v is Session | null {
  if (v === null) return true;
  if (typeof v !== "object") return false;
  const s = v as Partial<Session>;
  return typeof s.userKey === "string" && typeof s.user === "object" && s.user !== null && typeof s.user.user_id === "string";
}

const SESSION_KEY = "memory-panel.session";

// Before 0.4.3 the key was kept in localStorage, where it outlived the tab.
remove(SESSION_KEY, "local");

/** The user key stays in this tab: closing it signs out. */
export const sessionStore = createPersistedStore<Session | null>(SESSION_KEY, null, isSession, "session");

export function isSystemAdmin(session: Session | null | undefined): boolean {
  return session?.user.user_type === "system_admin";
}

/** v1 permissions: system admins configure and edit, everyone else reads. */
export function useIsAdmin(): boolean {
  return isSystemAdmin(useStore(sessionStore));
}

export function signOut(): void {
  sessionStore.set(null);
}
