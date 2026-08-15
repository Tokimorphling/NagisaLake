"""ComfyUI custom-node entrypoint for the Nagisalake Worker bridge.

Install the Rust extension with the adjacent ``pyproject.toml`` first, then
copy this directory into ComfyUI's ``custom_nodes`` directory.
"""

from nagisalake_comfyui import (  # noqa: F401
    NODE_CLASS_MAPPINGS,
    NODE_DISPLAY_NAME_MAPPINGS,
)

__all__ = ["NODE_CLASS_MAPPINGS", "NODE_DISPLAY_NAME_MAPPINGS"]
