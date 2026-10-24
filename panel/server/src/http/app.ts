import { Hono } from "hono";
import { bodyLimit } from "hono/body-limit";
import { secureHeaders } from "hono/secure-headers";
import type { UserKeyVerifier } from "../auth/user-key-verifier.js";
import type { CoreClient } from "../core-client/port.js";
import type { AppEnv } from "./app-env.js";
import { errorEnvelope } from "./envelope.js";
import { handleError } from "./middleware/error-handler.js";
import { requestId } from "./middleware/request-id.js";
import { coreProxyRoutes } from "./routes/core-proxy.js";
import { staticUiRoutes } from "./routes/static-ui.js";
import { systemRoutes, type PublicConfig } from "./routes/system.js";

export interface AppDeps {
  readonly core: CoreClient;
  readonly verifier: UserKeyVerifier | undefined;
  readonly publicConfig: PublicConfig;
  readonly uiDistDir: string | undefined;
}

const MAX_BODY_BYTES = 4 * 1024 * 1024;

/** Composes the BFF. Pure: no listening socket, so tests use `app.request()`. */
export function createApp(deps: AppDeps): Hono<AppEnv> {
  const app = new Hono<AppEnv>();
  app.use("*", requestId());
  app.use(
    "*",
    secureHeaders({
      crossOriginEmbedderPolicy: false,
      // The UI is one bundled script and stylesheet from this origin; nothing
      // else may run, load or frame it.
      contentSecurityPolicy: {
        defaultSrc: ["'self'"],
        scriptSrc: ["'self'"],
        styleSrc: ["'self'"],
        imgSrc: ["'self'", "data:"],
        fontSrc: ["'self'"],
        connectSrc: ["'self'"],
        objectSrc: ["'none'"],
        baseUri: ["'none'"],
        formAction: ["'self'"],
        frameAncestors: ["'none'"],
      },
    }),
  );
  app.use(
    "/api/*",
    bodyLimit({
      maxSize: MAX_BODY_BYTES,
      onError: (c) => c.json(errorEnvelope(413, "PAYLOAD_TOO_LARGE", c.get("requestId")), 413),
    }),
  );
  app.onError(handleError);

  app.route("/", systemRoutes(deps.publicConfig));
  app.route("/", coreProxyRoutes({ core: deps.core, verifier: deps.verifier }));
  app.route("/", staticUiRoutes(deps.uiDistDir));
  return app;
}
