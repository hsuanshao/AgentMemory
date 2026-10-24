"""AgentMemory provider (v2) — a thin adapter over the `amem` client.

All the work lives in `amem` so every host behaves the same: it holds the
server URL, user key, team and agent (`amem login`), redacts secrets, and
queues captures while the server is unreachable.

Flow: prefetch → `amem recall`; sync_turn → `amem capture --format openai`
with the messages added since the previous turn (tool calls included). One
background worker sends turns in order, so the turn never waits on the
network and turns never overtake each other.

Settings: `amem login` (shared with Claude Code / Antigravity), or the
AMEM_* environment variables. AMEM_BIN points at a non-PATH binary.
"""

from __future__ import annotations

import json
import logging
import os
import queue
import shutil
import subprocess
import threading
from typing import Any, Optional

from agent.memory_provider import MemoryProvider

logger = logging.getLogger(__name__)

_RECALL_TIMEOUT = 10.0
_CAPTURE_TIMEOUT = 60.0


def _amem() -> Optional[str]:
    return os.environ.get("AMEM_BIN") or shutil.which("amem")


def _run(args: list[str], *, stdin: Optional[str] = None, timeout: float) -> str:
    binary = _amem()
    if not binary:
        raise RuntimeError("amem not found on PATH")
    done = subprocess.run(
        [binary, *args], input=stdin, capture_output=True, text=True, timeout=timeout, check=False
    )
    if done.returncode != 0:
        raise RuntimeError(done.stderr.strip() or f"amem exited with {done.returncode}")
    return done.stdout


class AgentMemoryProvider(MemoryProvider):
    def __init__(self) -> None:
        self._session_id = "hermes"
        self._primary = True
        # messages already captured, per session
        self._sent: dict[str, int] = {}
        self._lock = threading.Lock()
        self._jobs: "queue.Queue[Optional[tuple[str, str]]]" = queue.Queue()
        self._worker: Optional[threading.Thread] = None

    @property
    def name(self) -> str:
        return "agentmemory"

    def is_available(self) -> bool:
        return _amem() is not None

    def unavailable_reason(self) -> str:
        return "install the `amem` client and run `amem login` (see docs/REMOTE-CLIENTS.md)"

    def initialize(self, session_id: str, **kwargs) -> None:
        self._session_id = session_id or "hermes"
        # subagents, cron and flush contexts must not write memory
        self._primary = kwargs.get("agent_context", "primary") == "primary"

    def on_session_switch(self, new_session_id: str, *, parent_session_id: str = "",
                          reset: bool = False, rewound: bool = False, **kwargs) -> None:
        with self._lock:
            if reset or rewound:
                self._sent.pop(new_session_id, None)
        self._session_id = new_session_id

    def shutdown(self) -> None:
        if self._worker and self._worker.is_alive():
            self._jobs.put(None)
            self._worker.join(timeout=_CAPTURE_TIMEOUT)

    def _drain(self) -> None:
        while (job := self._jobs.get()) is not None:
            sid, payload = job
            try:
                _run(["capture", "--format", "openai", "--session", sid],
                     stdin=payload, timeout=_CAPTURE_TIMEOUT)
            except Exception as exc:  # amem queues undeliverable turns itself
                logger.warning("agentmemory capture failed (session=%s): %s", sid, exc)

    # ── read path ──────────────────────────────────────────────────────
    def prefetch(self, query: str, *, session_id: str = "") -> str:
        if not query or not query.strip():
            return ""
        try:
            return _run(["recall", "--", query], timeout=_RECALL_TIMEOUT).strip()
        except Exception as exc:  # never break the turn over memory
            logger.debug("agentmemory recall failed: %s", exc)
            return ""

    # ── write path ─────────────────────────────────────────────────────
    def _new_messages(self, sid: str, user_content: str, assistant_content: str,
                      messages: Optional[list[dict[str, Any]]]) -> list[dict[str, Any]]:
        fallback = [{"role": "user", "content": user_content},
                    {"role": "assistant", "content": assistant_content}]
        if not messages:
            return fallback
        with self._lock:
            start = self._sent.get(sid, 0)
            self._sent[sid] = len(messages)
        if start > len(messages):  # history was compressed or rewound
            return fallback
        return messages[start:]

    def sync_turn(self, user_content: str, assistant_content: str, *,
                  session_id: str = "", messages=None, turn_author=None) -> None:
        if not self._primary:
            return
        sid = session_id or self._session_id
        new = self._new_messages(sid, user_content, assistant_content, messages)
        if not new:
            return
        self._jobs.put((sid, json.dumps(new, ensure_ascii=False, default=str)))
        with self._lock:
            if self._worker is None or not self._worker.is_alive():
                self._worker = threading.Thread(target=self._drain, name="agentmemory-capture", daemon=True)
                self._worker.start()

    # ── explicit search tool ───────────────────────────────────────────
    def get_tool_schemas(self) -> list[dict]:
        return [{
            "name": "agentmemory_search",
            "description": ("Search long-term memory (facts, user preferences, past decisions, "
                            "persona) shared across sessions, machines and agents."),
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "what to look for"},
                },
                "required": ["query"],
            },
        }]

    def handle_tool_call(self, tool_name: str, args: dict, **kwargs) -> str:
        if tool_name != "agentmemory_search":
            return json.dumps({"error": f"unknown tool {tool_name}"})
        try:
            data = json.loads(_run(["recall", "--json", "--", str(args.get("query", ""))],
                                   timeout=_RECALL_TIMEOUT))
            return json.dumps({"memories": data.get("memories", []),
                               "strategy": data.get("strategy")}, ensure_ascii=False)
        except Exception as exc:
            return json.dumps({"error": str(exc)})


def register(ctx) -> None:
    """Register AgentMemory as a memory provider plugin."""
    ctx.register_memory_provider(AgentMemoryProvider())
