# Running the server and the clients on different machines

English | [繁體中文](./REMOTE-CLIENTS.zh-tw.md)

This guide covers the common deployment: **one Agent Memory server** (memory-core + Memory Panel) on a host you control, and **agents on other machines** — Claude Code, Codex, Antigravity, Hermes — that read and write memory over the network.

```text
 laptop / workstation                              server host
┌───────────────────────────────┐   HTTPS   ┌──────────────────────────────┐
│ Claude Code ─┐                │
│ Codex ───────┤                │           │ reverse proxy (TLS)          │
│ Antigravity ─┼─ amem ─────────┼──────────►│   └─► memory-core :8420      │
│ Hermes ──────┘  (user key)    │           │ Memory Panel :8125 (admins)  │
│  ~/.config/agent-memory/      │           │ SQLite or Postgres (+Redis)  │
│  ~/.local/share/agent-memory/ │           └──────────────────────────────┘
└───────────────────────────────┘
```

- Every client machine runs one small binary, **`amem`**. The host integrations (Claude Code, Codex and Antigravity plugins, a Hermes memory provider) only call `amem`. The binary keeps the settings, redacts secrets, and queues writes while the server is unreachable.
- Clients authenticate with a **personal user key** (`sk-mem-…`). They never need the server's `AGENT_MEMORY_API_KEY`.

## Contents

1. [How access works](#1-how-access-works)
2. [Prepare the server](#2-prepare-the-server)
3. [Create users, teams and agents (admin)](#3-create-users-teams-and-agents-admin)
4. [Install `amem` on each client machine](#4-install-amem-on-each-client-machine)
5. [Claude Code](#5-claude-code)
6. [Antigravity](#6-antigravity)
7. [Codex CLI](#7-codex-cli)
8. [Hermes Agent](#8-hermes-agent)
9. [Upgrading from the previous version](#9-upgrading-from-the-previous-version)
10. [What is sent, and what is not](#10-what-is-sent-and-what-is-not)
11. [Troubleshooting](#11-troubleshooting)

---

## 1. How access works

The server has two trust zones:

| Zone | Routes | Credential |
| --- | --- | --- |
| **User-scoped** | memory (L0–L3), `recall`, `session/add`, `skill/*` | `x-memory-user-key` only |
| **Bearer-scoped** | pipeline/stats, logs, prompts, `meta/*`, `internal/*` | `Authorization: Bearer <AGENT_MEMORY_API_KEY>` |

Every memory call names a **team** and an **agent**:

- **team_id and agent_id are required.** There is no `default` bucket any more. The agent must belong to the team.
- **Membership:** the user must be an active member of the team.
- **Own memory only:** members read and write only their own memory (`user_id` defaults to the caller). Only system admins can name another user.
- **What is isolated where:** L0 conversations and L1 atoms are separate per team + agent + user. L2 scenes and the L3 persona are shared by everyone using the same team + agent.

> **Choosing agents.** Using the same agent from several tools (e.g. Claude Code *and* Hermes) gives them one shared set of scenes and one persona. Use separate agents (`claude-code`, `hermes`, …) if you want each tool to build its own.

## 2. Prepare the server

Install as described in the [README](../README.md#quick-start) (`./install.sh`). For remote clients, the core API must be reachable from the client machines.

1. **Put TLS in front of port 8420.** User keys travel in a header; do not send them over plain HTTP outside a trusted network. A minimal [Caddy](https://caddyserver.com/) site:

   ```caddyfile
   memory.example.com {
       reverse_proxy 127.0.0.1:8420
   }
   ```

   Alternatively keep the server on a private network (VPN, Tailscale, WireGuard) and use its private address.

2. **Bind address.** With `docker compose` the port is already published. Running the binary directly, set `AGENT_MEMORY_HOST=0.0.0.0`; the server refuses non-loopback binds without `AGENT_MEMORY_API_KEY`.

3. **Keep the Panel private.** Only admins need it (port 8125). Publish it on an internal address or behind your SSO/VPN rather than the open internet.

4. **Check from a client machine:**

   ```bash
   curl -s https://memory.example.com/health
   ```

## 3. Create users, teams and agents (admin)

Sign in to the Memory Panel with the admin user key that `install.sh` printed. (To decide how many teams and agents you need, and what each one shares, see [MULTI-TENANCY.md](MULTI-TENANCY.md).) Then, in **Metadata**:

1. **Users → New user.** Copy the generated `sk-mem-…` key now; it is shown only once. Under the user's **Keys** you can issue more keys (one per machine is a good habit) and revoke them later.
2. **Teams → New team.** Pick an owner (the owner becomes a team admin).
3. Select the team → **Members → Assign user.** Add each user who should share this team's memory.
4. Select the team → **Agents → New agent.** For example `claude-code`, `antigravity`, `hermes`.

Give each user four values: **server URL**, **user key**, **team_id** (`team-…`) and **agent_id** (`agt-…`). The ids are shown in the Teams and Agents tables. If the server uses a memory instance other than `default`, include the **service id** too.

Users who are not admins can sign in to the Panel with their own key and browse their own memory read-only.

## 4. Install `amem` on each client machine

`amem` is a single binary (`crates/memory-client`). Each [release](https://github.com/hsuanshao/AgentMemory/releases) from 0.2.0 on ships it prebuilt for Linux (x86_64 and arm64, statically linked) and macOS (Apple silicon and Intel):

```bash
VERSION=0.2.2
PLATFORM=linux-x86_64   # or linux-arm64, macos-arm64, macos-x86_64
BASE=https://github.com/hsuanshao/AgentMemory/releases/download/v$VERSION
curl -fsSLO "$BASE/amem-$VERSION-$PLATFORM.tar.gz"
curl -fsSL "$BASE/SHA256SUMS" | grep " amem-$VERSION-$PLATFORM.tar.gz$" | shasum -a 256 -c
tar -xzf "amem-$VERSION-$PLATFORM.tar.gz"
mkdir -p ~/.local/bin
install -m 755 "amem-$VERSION-$PLATFORM/amem" ~/.local/bin/amem

amem --version
```

Or build it with a Rust toolchain:

```bash
# from a checkout of this repository
cargo install --path crates/memory-client

# or straight from git
cargo install --git https://github.com/hsuanshao/AgentMemory --tag v0.2.2 memory-client
```

Make sure the directory holding `amem` (`~/.local/bin` above, `~/.cargo/bin` for `cargo install`) is on the `PATH` that your agent tools see. Desktop apps sometimes start with a shorter `PATH` than your shell. On macOS, a file downloaded with a browser rather than `curl` is quarantined; clear it with `xattr -d com.apple.quarantine ~/.local/bin/amem`.

Then log in once. `--user-key -` reads the key from stdin, so it stays out of your shell history:

```bash
amem login --server-url https://memory.example.com \
           --team-id team-xxxxxxxxxxxx --agent-id agt-xxxxxxxxxxxx \
           --user-key -
# paste the sk-mem-… key, then press Enter
```

`login` makes a test recall before saving, so a wrong key, a team you don't belong to, or an agent from another team is reported immediately. The settings go to `~/.config/agent-memory/client.json`, readable only by you.

```bash
amem status                 # settings (key masked) and queue size
amem recall "release plan"  # prints what an agent would receive
```

All hosts on this machine share these settings. You can override them per process with `AMEM_SERVER_URL`, `AMEM_USER_KEY`, `AMEM_TEAM_ID`, `AMEM_AGENT_ID` and `AMEM_SERVICE_ID`. To give each tool its own agent, use the plugin settings below or set `AMEM_AGENT_ID` in that tool's environment.

## 5. Claude Code

The plugin lives in `clients/claude-code` and is published through this repository's marketplace (`.claude-plugin/marketplace.json`).

```bash
claude plugin marketplace add hsuanshao/AgentMemory
claude plugin install agent-memory@agent-memory
```

From a local checkout you can instead run `claude plugin marketplace add /path/to/AgentMemory`.

When the plugin is enabled, Claude Code offers its settings: server URL, user key (stored in the system keychain), team ID, agent ID and service ID. **Leave them empty to use `amem login`**, or fill them in to override it for Claude Code only. A typical use is a separate `agent_id`.

What the plugin does:

| Hook | Behaviour |
| --- | --- |
| `UserPromptSubmit` | Recalls memories for the prompt and adds them as context (at most ~9,500 characters). |
| `Stop` | Sends the turn: your prompt, the replies, and every tool call and result. Only lines added since the last Stop are sent. |
| MCP server `agent-memory` | Tool `memory_recall`, so Claude can search memory on purpose. |

A failing server never blocks Claude Code. The hooks exit silently and log to `~/.local/share/agent-memory/amem.log`.

## 6. Antigravity

The plugin lives in `clients/antigravity` (`plugin.json`, `hooks.json`, `mcp_config.json`). Install it from a checkout:

```bash
agy plugin install /path/to/AgentMemory/clients/antigravity
agy plugin list
```

Or copy the directory to `~/.gemini/config/plugins/agent-memory/`.

What it does:

| Hook | Behaviour |
| --- | --- |
| `PreInvocation` | On the first model call of each turn (`invocationNum == 0`), recalls memories for your latest request and injects them as an ephemeral message. |
| `Stop` | Reads the conversation transcript and sends the steps added since the last Stop, tool calls included. |
| MCP server `agent-memory` | Tool `memory_recall`. |

Notes:

- **Transcript format.** Antigravity does not document its transcript format. `amem` reads the format current Antigravity versions write (`USER_INPUT`, `PLANNER_RESPONSE`, tool steps). If an update changes it, capture stops and `amem.log` says why; recall keeps working.
- **Existing MCP file.** If you add the MCP server to `~/.gemini/config/mcp_config.json` by hand, make sure the file is valid JSON. An empty file is not.

## 7. Codex CLI

The plugin lives in `clients/codex` (`.codex-plugin/plugin.json`, `hooks/hooks.json`, `.mcp.json`) and is published through this repository's Codex marketplace (`.agents/plugins/marketplace.json`).

```bash
codex plugin marketplace add hsuanshao/AgentMemory      # or a local checkout: /path/to/AgentMemory
codex plugin add agent-memory@agent-memory
```

Then start `codex` once and run **`/hooks`** to review and trust the plugin's three hooks. Codex never runs a hook it has not been told to trust, so until you do this nothing is recalled or captured. In automation, `codex exec --dangerously-bypass-hook-trust` skips the review for that run.

The plugin uses `amem login`'s settings. To give Codex its own agent, start it with `AMEM_AGENT_ID=agt-…` in the environment; the hooks and the MCP server inherit it.

What the plugin does:

| Hook | Behaviour |
| --- | --- |
| `UserPromptSubmit` | Recalls memories for the prompt and adds them as context. The plugin raises Codex's per-hook limit to 4,000 tokens (`additionalContextLimit`). Also records the prompt for the turn. |
| `PostToolUse` | Records each tool call and its result. |
| `Stop` | Sends the turn: the prompt, the tool calls and results, and the final reply. A turn interrupted before `Stop` is sent with the next one. |
| MCP server `agent-memory` | Tool `memory_recall`. |

Notes:

- **No transcript parsing.** Codex documents its session files as unstable, so `amem` doesn't read them. It assembles the turn from the documented hook payloads, kept in `~/.local/share/agent-memory/state/codex/` until `Stop`. Only the final reply is captured, not the short messages Codex writes between tool calls.
- **Codex's own memories** are separate from Agent Memory. They don't interfere with each other.

## 8. Hermes Agent

The provider lives in `clients/hermes/agentmemory` and keeps the provider name `agentmemory`, so an existing `memory.provider: agentmemory` setting stays valid.

```bash
# 1. remove an older copy inside the Hermes source tree (it would take precedence)
rm -rf ~/.hermes/hermes-agent/plugins/memory/agentmemory

# 2. install the provider as a user plugin (survives Hermes updates)
mkdir -p ~/.hermes/plugins
cp -r /path/to/AgentMemory/clients/hermes/agentmemory ~/.hermes/plugins/

# 3. select it (skip if already set)
hermes config set memory.provider agentmemory
hermes memory status
```

Hermes reads `amem login`'s settings. The provider:

- `prefetch` → `amem recall` before each turn.
- `sync_turn` → `amem capture --format openai` with the messages added since the previous turn, tool calls included. One background worker sends them in order, so a turn never waits on the network.
- Skips writes for sub-agents, cron and flush contexts.
- Adds the tool `agentmemory_search`.

If `amem` is not on the `PATH` Hermes sees, set `AMEM_BIN=/full/path/to/amem` in Hermes's environment. The previous provider's `AGENTMEMORY_API_KEY`, `AGENTMEMORY_BASE_URL` and `AGENTMEMORY_SERVICE_ID` entries in `~/.hermes/.env` are no longer read and can be removed.

## 9. Upgrading from the previous version

Before this version, clients could write without naming a team or agent. That data sits in the legacy `default` buckets, which the data plane no longer serves. On the server, run `./upgrade.sh` from the updated install directory. It backs up the data, rebuilds, moves the old data to a team and agent of your choice, and prints the key and `amem login` line for the clients. [UPGRADE.md](UPGRADE.md) describes every step, the same steps by hand, and how to roll back. Then configure the clients as in sections 4–8.

To remove the client from a machine later, run `./uninstall.sh client` from a checkout. Add `--dry-run` first to see what it would remove. See [UNINSTALL.md](UNINSTALL.md).

## 10. What is sent, and what is not

- **Text** (your messages and the agent's replies) goes to L0. The server's pipeline distills it into atoms (L1), scenes (L2) and a persona (L3).
- **Tool calls and results** go to the skill buffer, so recurring tool-heavy work can become reusable skills. Each tool result is cut to its first 8,000 characters; each message to 32,000.
- **Secrets are redacted on your machine before anything is queued or sent.** Covered:
  - private key blocks
  - credentials in URLs
  - JWTs
  - Anthropic, OpenAI and memory user keys
  - GitHub, Slack, AWS and Google keys
  - bearer tokens
  - `password=` / `token:` / `api_key` style assignments

  Matches become `[REDACTED:<kind>]`. Redaction is pattern-based; treat it as a safety net, not a guarantee.
- **Offline queue.** If the server is unreachable (or the key is rejected), captures wait in `~/.local/share/agent-memory/queue/`, readable only by you. They are sent in order on the next turn, or run `amem flush`. At most 2,000 requests are kept; the oldest are dropped first. Requests the server rejects as malformed move to `queue/failed/` for inspection.
- **Sub-agents** (Claude Code side chains, Hermes sub-agents) are not captured.

## 11. Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `amem login` → `HTTP 401` | Wrong or revoked user key. Issue a new key in the Panel (Users → Keys). |
| `HTTP 403 agent does not belong to the team` | `agent_id` is from another team. Check the Agents table of your team. |
| `HTTP 403 not an active member of the team` | Ask an admin to assign you to the team. |
| `HTTP 400 team_id is required` | A client without team/agent settings. Run `amem login` or fill in the plugin settings. |
| `amem status` shows a growing queue | The server is unreachable from this machine. Check the URL and TLS, then run `amem flush`. |
| Nothing is recalled | New memories appear after the server's pipeline distills them, which takes up to a few minutes. Check the Panel dashboard for pipeline errors (for example, the server's LLM is not configured). |
| Hooks seem to do nothing | Look at `~/.local/share/agent-memory/amem.log`; hooks never fail loudly. Make sure `amem` is on the host's `PATH`. |
| Hermes still uses the old provider | An older copy in `~/.hermes/hermes-agent/plugins/memory/agentmemory` takes precedence. Remove it (section 7). |

Files on a client machine:

| Path | Content |
| --- | --- |
| `~/.config/agent-memory/client.json` | Settings from `amem login` (0600). Override with `AMEM_CONFIG`. |
| `~/.local/share/agent-memory/queue/` | Offline queue (0600). Override the base with `AMEM_DATA_DIR`. |
| `~/.local/share/agent-memory/state/` | Per-session cursors, so each turn is sent once. |
| `~/.local/share/agent-memory/amem.log` | Hook and capture errors. |
