# syntax=docker/dockerfile:1
# ──────────────────────────────────────────────────────────────────────
# memory-core (Rust) — production image with LLM CLIs pre-installed
#
#   docker build -f docker/core.Dockerfile -t memory-core:local .
#   docker run --rm -p 8420:8420 \
#     -e AGENT_MEMORY_API_KEY=change-me \
#     -e AGENT_MEMORY_LLM_PROVIDER=cli -e AGENT_MEMORY_LLM_CLI_COMMAND=claude \
#     -e AGENT_MEMORY_LLM_CLI_ARGS=-p -e ANTHROPIC_API_KEY=sk-ant-... \
#     -v memory-core-data:/data memory-core:local
#
# Bundled CLIs (official native installers, no npm; all verify checksums):
#   claude  Claude Code      https://claude.ai/install.sh
#   agy     Antigravity CLI  https://antigravity.google/cli/install.sh
#   codex   Codex CLI        https://chatgpt.com/codex/install.sh
# Skip any with --build-arg INSTALL_CLAUDE=false / INSTALL_ANTIGRAVITY=false /
# INSTALL_CODEX=false.
# ──────────────────────────────────────────────────────────────────────

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
# Cache the registry and target dir across builds (BuildKit). Cargo decides
# what to rebuild by modification time, and sources from an archive (git
# archive, a release tarball) carry their commit time, which can be older than
# the cached build: cargo would then ship the previous binary. This step only
# runs when crates/ changed (BuildKit compares content), so touching the
# sources costs nothing otherwise and forces the workspace crates to rebuild.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    find crates -type f -exec touch {} + \
 && cargo build --release --locked -p memory-server \
 && cp target/release/memory-server /usr/local/bin/memory-server

FROM debian:bookworm-slim AS runtime
# apt goes over HTTPS, as some networks block outbound HTTP. The slim image
# has no CA certificates until ca-certificates is installed, so the Rust
# image's bundle verifies the mirror for this install; apt checks every
# package's signature either way. Copied from the image, not the build
# stage, so this stage does not wait for cargo and still runs alongside it.
COPY --from=rust:1-slim-bookworm /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
# curl downloads the CLI installers; git is used by coding CLIs inside a project.
RUN sed -i 's|http://deb.debian.org|https://deb.debian.org|' /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --home-dir /home/memory --uid 10001 --shell /bin/bash memory \
 && mkdir -p /data /config /workspace \
 && chown memory:memory /data /workspace

# The CLIs install into the user's home (~/.local/bin) and refuse to run
# as root, so install as the service user. The home directory is kept
# separate from the /data volume so a mounted volume never hides them.
USER memory
ENV PATH="/home/memory/.local/bin:${PATH}"
ARG INSTALL_CLAUDE=true
ARG CLAUDE_VERSION=stable
ARG INSTALL_ANTIGRAVITY=true
ARG INSTALL_CODEX=true
RUN set -eu; \
    if [ "$INSTALL_CLAUDE" = "true" ]; then \
      curl -fsSL https://claude.ai/install.sh -o /tmp/install-claude.sh; \
      bash /tmp/install-claude.sh "$CLAUDE_VERSION"; \
      rm /tmp/install-claude.sh; \
      claude --version; \
    fi; \
    if [ "$INSTALL_ANTIGRAVITY" = "true" ]; then \
      curl -fsSL https://antigravity.google/cli/install.sh -o /tmp/install-agy.sh; \
      bash /tmp/install-agy.sh; \
      rm /tmp/install-agy.sh; \
      test -x /home/memory/.local/bin/agy; \
    fi; \
    if [ "$INSTALL_CODEX" = "true" ]; then \
      curl -fsSL https://chatgpt.com/codex/install.sh -o /tmp/install-codex.sh; \
      # The binary goes under CODEX_HOME; keep it out of ~/.codex, where the
      # login volume is mounted and would hide it.
      CODEX_HOME=/home/memory/.codex-cli sh /tmp/install-codex.sh; \
      rm /tmp/install-codex.sh; \
      codex --version; \
    fi; \
    # Pre-create the login directories so volumes mounted there stay
    # owned by the service user (Docker would create them as root).
    mkdir -p /home/memory/.claude /home/memory/.gemini /home/memory/.codex

COPY --from=build /usr/local/bin/memory-server /usr/local/bin/memory-server
COPY config/memory-core.example.yaml /config/memory-core.yaml

ENV AGENT_MEMORY_CONFIG=/config/memory-core.yaml \
    AGENT_MEMORY_HOST=0.0.0.0 \
    AGENT_MEMORY_PORT=8420 \
    AGENT_MEMORY_DATA_DIR=/data \
    LOG_FORMAT=json
# Mount a project at /workspace and set AGENT_MEMORY_LLM_CLI_WORKDIR=/workspace
# to let the CLI see that project.
VOLUME ["/data"]
EXPOSE 8420
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD ["memory-server", "healthcheck"]
ENTRYPOINT ["memory-server"]
