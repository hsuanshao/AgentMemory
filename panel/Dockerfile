# syntax=docker/dockerfile:1

# ---- build: install all deps, build the SPA and compile the BFF ----
FROM node:22-slim AS build
WORKDIR /app
COPY package.json package-lock.json ./
COPY server/package.json server/
COPY web/package.json web/
RUN npm ci --no-audit --no-fund
COPY server server
COPY web web
RUN npm run build

# ---- deps: production dependencies of the server only ----
FROM node:22-slim AS deps
WORKDIR /app
COPY package.json package-lock.json ./
COPY server/package.json server/
COPY web/package.json web/
RUN npm ci --omit=dev --workspace server --include-workspace-root=false --no-audit --no-fund

# ---- runtime ----
FROM node:22-slim AS runtime
ENV NODE_ENV=production \
    PORT=8125 \
    HOST=0.0.0.0 \
    CORE_URL=http://127.0.0.1:8420 \
    CORE_DEPARTMENT_ID=default \
    UI_DIST_DIR=/app/web/dist
WORKDIR /app
COPY --from=deps --chown=node:node /app/node_modules ./node_modules
COPY --from=build --chown=node:node /app/server/package.json ./server/package.json
COPY --from=build --chown=node:node /app/server/dist ./server/dist
COPY --from=build --chown=node:node /app/web/dist ./web/dist
USER node
EXPOSE 8125
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD node -e "fetch('http://127.0.0.1:'+(process.env.PORT||8125)+'/health').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"
CMD ["node", "server/dist/index.js"]
