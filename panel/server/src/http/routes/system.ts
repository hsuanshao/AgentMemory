import { Hono } from "hono";
import type { AppEnv } from "../app-env.js";

export interface PublicConfig {
  readonly department_id: string;
  readonly core_url_display: string;
}

/** `/health` (panel liveness) and `/api/config` (non-secret UI config). */
export function systemRoutes(publicConfig: PublicConfig): Hono<AppEnv> {
  const app = new Hono<AppEnv>();
  app.get("/health", (c) => c.json({ status: "ok" }));
  app.get("/api/config", (c) => c.json(publicConfig));
  return app;
}
