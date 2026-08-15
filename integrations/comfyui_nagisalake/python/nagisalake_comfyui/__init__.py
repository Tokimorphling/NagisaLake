"""ComfyUI nodes which start a Nagisalake reverse-connected worker.

The node only owns the long-lived Hub registration. It does not submit the
current ComfyUI graph or create a DispatchJob. Consumers continue to submit
jobs through the Hub API, and the Rust worker executes configured workflows
against this local ComfyUI instance.
"""

from __future__ import annotations

import atexit
import logging
import os
import threading
from typing import Optional

from ._nagisalake_worker import WorkerHandle, start_worker, worker_version

_LOGGER = logging.getLogger(__name__)
_LOCK = threading.Lock()
_HANDLE: Optional[WorkerHandle] = None


def start(config_path: Optional[str] = None) -> WorkerHandle:
    """Start or reuse the process-wide reverse connection."""

    global _HANDLE
    with _LOCK:
        if _HANDLE is not None and _HANDLE.is_running():
            _LOGGER.info("reusing running Nagisalake Hub worker")
            return _HANDLE
        _LOGGER.info(
            "starting Nagisalake Hub worker with config %s",
            config_path or os.environ.get("NAGISALAKE_WORKER_CONFIG") or "<missing>",
        )
        _HANDLE = start_worker(config_path)
        _LOGGER.info(
            "Nagisalake Hub worker started: %s (%s)",
            _HANDLE.status(),
            _HANDLE.config_path,
        )
        return _HANDLE


def stop() -> None:
    """Stop the process-wide reverse connection, if one is running."""

    global _HANDLE
    with _LOCK:
        if _HANDLE is not None:
            _LOGGER.info("stopping Nagisalake Hub worker")
            _HANDLE.stop()
            _HANDLE = None
            _LOGGER.info("Nagisalake Hub worker stopped")


def status() -> str:
    """Return the current process-wide worker status."""

    with _LOCK:
        current = "stopped" if _HANDLE is None else _HANDLE.status()
        _LOGGER.info("Nagisalake Hub worker status: %s", current)
        return current


class NagisalakeHubWorker:
    """Connect this ComfyUI graph to a configured Nagisalake Hub worker."""

    @classmethod
    def INPUT_TYPES(cls):
        return {
            "required": {
                "config_path": (
                    "STRING",
                    {
                        "default": os.environ.get("NAGISALAKE_WORKER_CONFIG", ""),
                        "multiline": False,
                    },
                ),
            }
        }

    RETURN_TYPES = ("STRING",)
    RETURN_NAMES = ("status",)
    FUNCTION = "connect"
    CATEGORY = "Nagisalake/Worker"
    OUTPUT_NODE = True

    def connect(self, config_path: str):
        handle = start(config_path or None)
        return (f"{handle.status()} ({handle.config_path})",)


class NagisalakeHubWorkerStop:
    """Stop the process-wide Hub connection when this node executes."""

    @classmethod
    def INPUT_TYPES(cls):
        return {"required": {}}

    RETURN_TYPES = ("STRING",)
    RETURN_NAMES = ("status",)
    FUNCTION = "disconnect"
    CATEGORY = "Nagisalake/Worker"
    OUTPUT_NODE = True

    def disconnect(self):
        stop()
        return ("stopped",)


NODE_CLASS_MAPPINGS = {
    "NagisalakeHubWorker": NagisalakeHubWorker,
    "NagisalakeHubWorkerStop": NagisalakeHubWorkerStop,
}

NODE_DISPLAY_NAME_MAPPINGS = {
    "NagisalakeHubWorker": "Nagisalake Hub Worker",
    "NagisalakeHubWorkerStop": "Nagisalake Hub Worker Stop",
}


@atexit.register
def _shutdown_worker() -> None:
    try:
        stop()
    except Exception:
        # Interpreter shutdown can tear down the logging and threading modules
        # before atexit handlers run. The Rust token is still cancelled by Drop.
        pass


__all__ = [
    "NagisalakeHubWorker",
    "NagisalakeHubWorkerStop",
    "NODE_CLASS_MAPPINGS",
    "NODE_DISPLAY_NAME_MAPPINGS",
    "WorkerHandle",
    "start",
    "stop",
    "status",
    "worker_version",
]
