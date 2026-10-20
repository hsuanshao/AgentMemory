# Memory Panel

Monitoring and administration console for the Rust **MemoryCore**.

```text
browser ──► panel/server (Hono BFF, :8125) ──► MemoryCore (:8420)
            · serves the React SPA
            · holds CORE_API_KEY (never sent to the browser)
            · forwards an allowlisted set of /v3 calls
```

* `server/`: Hono 4 BFF on Node 22 (ESM, strict TypeScript).
  * `config/` reads env config.
  * `core-client/` holds the `CoreClient` port and its fetch adapter.
  * `auth/` holds the user-key verifier.
  * `http/` holds the routes, middleware and allowlist.
* `web/`: React 18, Vite, Tailwind and react-router (hash router).
  * `api/` is the typed client.
  * `features/<page>/` holds the pages; `components/` and `hooks/` are shared.

## Pages

| Page | Core calls |
| --- | --- |
| Login | `meta/auth/verify`: checks `data.valid`, because an invalid key still returns HTTP 200 |
| Dashboard (auto-refresh every 5 s, can be paused) | `GET /health`, `pipeline/status`, `stats/overview`<br>Distillation card: `memory-generation-log/summary`, `memory-generation-log/list` (latest failures), `pipeline/dead-letter/list`; system admins: `pipeline/dead-letter/redrive` |
| Memory browser (team/agent/user scope) | L0 `conversation/{query,search,delete}`<br>L1 `atomic/{query,search,update,delete}`<br>L2 `scenario/{ls,read,write}`<br>L3 `core/{read,write}`<br>Recall test: `recall` |
| Generation logs (`#/logs?status=failed` opens filtered) | `memory-generation-log/{list,get}` |
| Skills | `skill/{list,search,get,versions}` |
| Metadata: Teams / Users / Assets | Everyone: `meta/team/list`, `meta/team-member/list`, `meta/agent/list`, `meta/asset/list-accessible`<br>System admins: `meta/user/{list,create,delete}`, `meta/user-key/{list,create,revoke}`, `meta/team/{create,update,delete}` (+ `list` with `all`), `meta/team-member/{add,remove}`, `meta/agent/{create,update,delete}` |

## BFF endpoints

| Method | Path | Description |
| --- | --- | --- |
| GET | `/health` | `{"status":"ok"}` (panel liveness) |
| GET | `/api/config` | `{department_id, core_url_display}` (contains no secrets) |
| GET | `/api/core-health` | Proxies Core `GET /health` |
| POST | `/api/core/<path>` | Forwards to Core `POST /v3/<path>` if `<path>` is allowlisted (see `server/src/http/allowlist.ts`) |
| GET | `/*` | SPA from `UI_DIST_DIR`, falling back to `index.html` |

The BFF adds these headers to every forwarded call:

* `Authorization: Bearer $CORE_API_KEY`
* `x-memory-department-id: $CORE_DEPARTMENT_ID`
* the browser's `x-memory-user-key`
* `x-request-id`, taken from the browser or generated

Core responses pass through unchanged. The BFF generates its own errors using Core's envelope:

| Case | Status | Body |
| --- | --- | --- |
| Path not allowlisted | 404 | `{"code":404,"message":"NOT_FOUND"}` |
| Core timeout | 504 | `{"code":504,"message":"CORE_TIMEOUT"}` |
| Core unreachable | 502 | `{"code":502,"message":"CORE_UNAVAILABLE"}` |
| Missing or invalid user key | 401 | Only when `PANEL_REQUIRE_USER_KEY` is on (the default) |

By default, every proxied call except `meta/auth/verify` needs a user key that Core accepts. Calls that change data or administer the department (`ADMIN_ONLY_V3_PATHS` in `server/src/http/allowlist.ts`) also need a `system_admin` key; anyone else gets `403 forbidden: system_admin_required`, and the UI hides those controls. Verdicts are cached for 60 s. Without this check, anyone who could reach the panel could use the Core API key it holds.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `PORT` | `8125` | Listen port |
| `HOST` | `0.0.0.0` | Listen address |
| `CORE_URL` | `http://127.0.0.1:8420` | MemoryCore base URL |
| `CORE_API_KEY` | (none) | Bearer token for Core (`AGENT_MEMORY_API_KEY` on the Core side) |
| `CORE_DEPARTMENT_ID` | `default` | Sent as `x-memory-department-id` |
| `CORE_TIMEOUT_MS` | `15000` | Timeout for each Core call |
| `UI_DIST_DIR` | (none) | Built SPA directory. The Docker image sets it to `/app/web/dist` |
| `PANEL_REQUIRE_USER_KEY` | `true` | Set to `false` to skip the BFF user-key check (local development only) |

## Run locally

```bash
cd panel
npm install
npm run build        # web (vite) + server (tsc)
npm test             # vitest for server and web

# production-style: one process serves the API and the SPA
CORE_URL=http://127.0.0.1:8420 CORE_API_KEY=change-me UI_DIST_DIR=web/dist npm start
# open http://localhost:8125

# development: BFF with tsx watch + Vite dev server (proxies /api to :8125)
CORE_URL=http://127.0.0.1:8420 CORE_API_KEY=change-me npm run dev
# open http://localhost:5173
```

## First login: bootstrap an admin

A fresh Core has no users. Create the first `system_admin` user with the internal endpoint, which only needs the Bearer key:

```bash
curl -s http://127.0.0.1:8420/v3/internal/meta/user/init-admin \
  -H "Authorization: Bearer $CORE_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"username":"admin"}'
# → {"code":0,"data":{"user_id":"…","user_key":"sk-mem-…"}}
```

Sign in to the panel with the returned `user_key`. A second call returns 409, because Core is already initialised.

## Docker

```bash
docker build -t memory-panel:dev panel

docker network create memory 2>/dev/null || true
# Core container, named "memory-core", listening on 8420 inside the network:
#   docker run -d --name memory-core --network memory -e AGENT_MEMORY_API_KEY=change-me … memory-core:dev

docker run -d --name memory-panel --network memory -p 8125:8125 \
  -e CORE_URL=http://memory-core:8420 \
  -e CORE_API_KEY=change-me \
  -e CORE_DEPARTMENT_ID=default \
  memory-panel:dev
```

The image is a multi-stage `node:22-slim` build:

* It runs as the non-root `node` user.
* It contains only the server's production dependencies plus the two `dist` folders.
* It exposes 8125 and has a `HEALTHCHECK` on `/health`.
