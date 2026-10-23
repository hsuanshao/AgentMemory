import { serve } from "@hono/node-server";
import { UserKeyVerifier } from "./auth/user-key-verifier.js";
import { displayCoreUrl, loadConfig } from "./config/config.js";
import { FetchCoreClient } from "./core-client/fetch-core-client.js";
import { createApp } from "./http/app.js";

function main(): void {
  const config = loadConfig();
  if (config.staleCoreApiKey) {
    console.warn(
      "[panel] CORE_API_KEY is set but no longer used: Core authorizes each signed-in user's key. Remove it so the panel does not hold the server API key.",
    );
  }
  const core = new FetchCoreClient({
    baseUrl: config.coreUrl,
    departmentId: config.coreDepartmentId,
    timeoutMs: config.coreTimeoutMs,
  });
  const app = createApp({
    core,
    verifier: config.requireUserKey ? new UserKeyVerifier(core) : undefined,
    publicConfig: { department_id: config.coreDepartmentId, core_url_display: displayCoreUrl(config.coreUrl) },
    uiDistDir: config.uiDistDir,
  });

  const server = serve({ fetch: app.fetch, port: config.port, hostname: config.host }, (info) => {
    console.log(`[panel] listening on http://${config.host}:${info.port} → core ${displayCoreUrl(config.coreUrl)}`);
  });

  const shutdown = (): void => {
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(0), 5_000).unref();
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}

main();
