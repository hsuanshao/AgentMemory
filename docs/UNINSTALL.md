# Uninstalling

[繁體中文](UNINSTALL.zh-tw.md)

Agent Memory has two parts that are removed separately. The **client** lives on each machine that runs an agent. The **server** lives on the machine that runs Docker Compose. `./uninstall.sh` removes either part. The steps below also show what it does, so you can do the same by hand.

## Contents

- [Quick reference](#quick-reference)
- [A. Remove the client](#a-remove-the-client)
- [B. Remove the server](#b-remove-the-server)
- [C. Troubleshooting](#c-troubleshooting)

## Quick reference

```bash
./uninstall.sh client                 # on a machine that runs agents
./uninstall.sh server                 # on the server, in the install directory; keeps the data
./uninstall.sh server --purge-data    # also deletes every memory, user and key
```

| Option | Meaning |
| --- | --- |
| `--dry-run` | Only print what would be removed |
| `--yes` | Do not ask before each part. `--purge-data` then skips its confirmation too |
| `--purge-data` | `server` only: also delete the Docker volumes (memories, users, keys, Claude/Antigravity/Codex CLI logins) |

Run the script from a checkout of this repository. Anything that isn't installed is skipped, so running it twice is harmless.

## A. Remove the client

```bash
./uninstall.sh client --dry-run   # look first
./uninstall.sh client
```

It removes the following:

| Part | What is removed | By hand |
| --- | --- | --- |
| Claude Code | The `agent-memory` plugin in every scope (local, project, user), the `agent-memory` marketplace and the plugin cache | `claude plugin uninstall agent-memory@agent-memory --scope <scope>`, `claude plugin marketplace remove agent-memory`, `rm -rf ~/.claude/plugins/cache/agent-memory` |
| Codex | The `agent-memory` plugin, its marketplace and the plugin cache | `codex plugin remove agent-memory@agent-memory`, `codex plugin marketplace remove agent-memory`, `rm -rf ~/.codex/plugins/cache/agent-memory` |
| Antigravity | The user plugin, plus the workspace copy in the directory you ran the script from | `agy plugin uninstall agent-memory`, `rm -rf ~/.gemini/config/plugins/agent-memory`, and in the project `rm -rf .agents/plugins/agent-memory` |
| Hermes | The provider installed from `clients/hermes` (`$HERMES_HOME/plugins/agentmemory`, default `~/.hermes`) | `rm -rf ~/.hermes/plugins/agentmemory` |
| `amem` | Its settings (including the user key), the offline queue and cursors, and the binary | `rm -rf ~/.config/agent-memory ~/.local/share/agent-memory`, then `cargo uninstall memory-client` |

Notes:

- **Unsent captures.** If the offline queue still holds turns, for example because the server was unreachable, the script warns before deleting them. Run `amem flush` first to send them.
- **Per-project installs** are found only in the directory you start the script from: Claude Code's `local` and `project` scopes, and Antigravity's `.agents/plugins/agent-memory`. Run the script from each such project, for example `cd ~/my-project && /path/to/AgentMemory/uninstall.sh client`, or remove them by hand.
- **Hermes.** The script only removes a provider that runs `amem`. An older provider with the same name, for example one inside the Hermes source tree, is left in place. Afterwards, choose another memory provider if `memory.provider` is still `agentmemory`: `hermes config set memory.provider <name>`.
- **Custom paths.** `AMEM_CONFIG`, `AMEM_DATA_DIR`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `CODEX_HOME` and `HERMES_HOME` are honoured, just as when the client is installed.
- **Server data is not touched.** The memories this machine sent stay on the server. To delete them, remove the user's data in the Panel, or remove the server (B).

## B. Remove the server

Run this in the server's install directory, the one holding `.env`:

```bash
./uninstall.sh server --dry-run   # look first
./uninstall.sh server             # keeps the data
```

What happens:

1. All containers of this install stop and are removed: memory-core, the Panel, and Postgres and Redis if you use those profiles. By hand: `docker compose --profile postgres --profile redis down --remove-orphans`.
2. The images built for it are removed: `memory-core:<MEMORY_IMAGE_TAG>` and `memory-panel:<MEMORY_IMAGE_TAG>`, where the tag defaults to `local`. By hand: `docker image rm memory-core:local memory-panel:local`. If another deployment still uses an image because it has the same tag, the image is kept and the script tells you.
3. **The data volumes are kept.** These hold the memories, users, keys and the Claude/Antigravity CLI logins. Running `./install.sh` again in the same directory, and choosing to reuse `.env`, brings everything back.

### Delete the data too

```bash
./uninstall.sh server --purge-data
```

This also deletes the volumes, by hand `docker compose --profile postgres --profile redis down --volumes`, and the `:pre-upgrade-…` images that `upgrade.sh` kept for a rollback (listed in `backups/*/images`). Every memory, user, key and CLI login of this install is gone for good. Because of that, the script asks you to type the install directory's name to confirm. Take a backup first if you might want the data back; see [UPGRADE.md, step B1](UPGRADE.md#b1-back-up).

If the install uses an external Postgres (`AGENT_MEMORY_POSTGRES_URL`), that database is not deleted. Drop it with your usual tools.

### Afterwards

- The directory still holds `.env` with the server API key, plus `backups/` and `upgrade-client-settings.txt` if you upgraded. Delete the directory when you no longer need it.
- Clients that still point at the server keep queueing captures locally. Remove them as in A.

## C. Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `No .env here` | `server` must run in the install directory. `cd` there, or call the script by its path from there |
| `Image … is still used by another container` | Another deployment on this machine uses the same `MEMORY_IMAGE_TAG`. It is kept on purpose; remove it once that deployment is gone |
| A plugin is still listed after `client` | It was installed under another name, or from another directory. Check `claude plugin list` / `agy plugin list` and remove it by hand as in the table |
| `Cancelled; nothing removed.` | The confirmation did not match the directory name. Run it again and type the name exactly |
