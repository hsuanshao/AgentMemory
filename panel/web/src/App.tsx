import { HashRouter, Navigate, Route, Routes } from "react-router-dom";
import { Layout } from "./components/Layout";
import { LoginPage } from "./features/auth/LoginPage";
import { sessionStore } from "./features/auth/session";
import { DashboardPage } from "./features/dashboard/DashboardPage";
import { LogsPage } from "./features/logs/LogsPage";
import { MemoryPage } from "./features/memory/MemoryPage";
import { MetaPage } from "./features/meta/MetaPage";
import { RedactionPage } from "./features/redaction/RedactionPage";
import { SkillsPage } from "./features/skills/SkillsPage";
import { useStore } from "./hooks/useStore";
import { useLocale } from "./i18n";

export function App() {
  // Subscribing here re-renders the whole tree when the language changes.
  useLocale();
  const session = useStore(sessionStore);
  if (!session) return <LoginPage />;

  return (
    <HashRouter>
      <Routes>
        <Route element={<Layout />}>
          <Route path="/dashboard" element={<DashboardPage />} />
          <Route path="/memory" element={<MemoryPage />} />
          <Route path="/logs" element={<LogsPage />} />
          <Route path="/skills" element={<SkillsPage />} />
          <Route path="/redaction" element={<RedactionPage />} />
          <Route path="/meta" element={<MetaPage />} />
          <Route path="*" element={<Navigate to="/dashboard" replace />} />
        </Route>
      </Routes>
    </HashRouter>
  );
}
