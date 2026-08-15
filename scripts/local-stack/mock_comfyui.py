#!/usr/bin/env python3
"""Minimal ComfyUI stand-in for exercising the Nagisalake dispatch chain.

Implements only the four endpoints the Worker actually calls, plus `/queue` and
`/system_stats`. It runs no inference: every prompt returns the same tiny PNG.
The point is to test Hub -> Worker -> engine -> object storage -> download
without a GPU or model weights.

Environment:
  MOCK_PORT            listen port (default 8188)
  MOCK_PENDING_POLLS   how many /history polls report "not done yet" before the
                       prompt completes. Raise it to hold a job in `running` so
                       queue behaviour can be observed; 2 is fast.
  MOCK_FAIL_PROMPTS    comma-separated 1-based prompt ordinals to fail, for
                       exercising the failure path (e.g. "2" fails the 2nd).

Only the endpoints below are implemented, so a Worker change that starts calling
something else will fail loudly here rather than silently pass.
"""

from __future__ import annotations

import base64
import json
import os
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PORT = int(os.environ.get("MOCK_PORT", "8188"))
PENDING_POLLS = int(os.environ.get("MOCK_PENDING_POLLS", "2"))
FAIL_PROMPTS = {
    int(value)
    for value in os.environ.get("MOCK_FAIL_PROMPTS", "").split(",")
    if value.strip().isdigit()
}

# 8x8 solid PNG.
PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAJUlEQVR42mNk"
    "YPjPgAaYGBgYGf4zMDAyMDAyMDAyMDAyMDAyAAB2LQP/pZbnJQAAAABJRU5ErkJggg=="
)

_LOCK = threading.Lock()
# prompt_id -> {"polls": int, "ordinal": int}
_PROMPTS: dict[str, dict[str, int]] = {}
_SUBMITTED = 0


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        print(f"mock-comfyui: {fmt % args}", flush=True)

    def _json(self, status: int, payload: dict) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        path = urlparse(self.path).path
        self.rfile.read(int(self.headers.get("Content-Length", 0)))

        if path == "/prompt":
            global _SUBMITTED
            prompt_id = str(uuid.uuid4())
            with _LOCK:
                _SUBMITTED += 1
                _PROMPTS[prompt_id] = {"polls": 0, "ordinal": _SUBMITTED}
            self._json(200, {"prompt_id": prompt_id, "number": 1, "node_errors": {}})
        elif path == "/upload/image":
            self._json(200, {"name": "input.png", "subfolder": "", "type": "input"})
        elif path == "/queue":
            # The Worker posts here to delete a queued prompt on cancellation.
            self._json(200, {})
        else:
            self._json(404, {"error": f"mock has no POST {path}"})

    def do_GET(self) -> None:
        parsed = urlparse(self.path)
        path = parsed.path

        if path.startswith("/history/"):
            self._history(path.rsplit("/", 1)[-1])
        elif path == "/queue":
            self._queue()
        elif path == "/view":
            self._view(parse_qs(parsed.query))
        elif path == "/system_stats":
            self._json(200, {"system": {"comfyui_version": "mock-0.0.0"}})
        else:
            self._json(404, {"error": f"mock has no GET {path}"})

    def _history(self, prompt_id: str) -> None:
        with _LOCK:
            state = _PROMPTS.get(prompt_id)
            if state is None:
                # Unknown prompt: an empty object reads as "still pending".
                self._json(200, {})
                return
            state["polls"] += 1
            polls = state["polls"]
            ordinal = state["ordinal"]

        if polls <= PENDING_POLLS:
            self._json(200, {})
            return

        if ordinal in FAIL_PROMPTS:
            self._json(
                200,
                {
                    prompt_id: {
                        "status": {
                            "status_str": "error",
                            "completed": False,
                            "messages": [["execution_error", {"exception_message": "mock failure"}]],
                        },
                        "outputs": {},
                    }
                },
            )
            return

        self._json(
            200,
            {
                prompt_id: {
                    "status": {"status_str": "success", "completed": True},
                    "outputs": {
                        "9": {
                            "images": [
                                {
                                    "filename": f"mock_{prompt_id[:8]}.png",
                                    "subfolder": "",
                                    "type": "output",
                                }
                            ]
                        }
                    },
                }
            },
        )

    def _queue(self) -> None:
        """Reports which prompts are executing versus waiting.

        Entries are `[number, prompt_id, prompt, extra_data, outputs]`, matching
        ComfyUI, because the Worker reads the prompt id positionally.
        """
        with _LOCK:
            live = [
                (prompt_id, state)
                for prompt_id, state in _PROMPTS.items()
                if state["polls"] <= PENDING_POLLS
            ]
        # ComfyUI executes serially: the oldest live prompt runs, the rest wait.
        live.sort(key=lambda item: item[1]["ordinal"])
        running = [[0, live[0][0], {}, {}, []]] if live else []
        pending = [
            [index + 1, prompt_id, {}, {}, []]
            for index, (prompt_id, _state) in enumerate(live[1:])
        ]
        self._json(200, {"queue_running": running, "queue_pending": pending})

    def _view(self, query: dict[str, list[str]]) -> None:
        if not query.get("filename"):
            self._json(400, {"error": "filename is required"})
            return
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(PNG)))
        self.end_headers()
        self.wfile.write(PNG)


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(
        f"mock ComfyUI on 127.0.0.1:{PORT} "
        f"(pending_polls={PENDING_POLLS}, fail={sorted(FAIL_PROMPTS) or 'none'})",
        flush=True,
    )
    server.serve_forever()
