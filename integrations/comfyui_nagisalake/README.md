# Nagisalake ComfyUI Worker Node

This integration puts the reverse-connected Worker bootstrap in a ComfyUI
workflow. The `Nagisalake Hub Worker` node starts one process-wide Tokio worker
when the graph executes; it does not submit the current graph and it does not
create a `DispatchJob`. The optional stop node cancels that connection.

## Build and install

Run these commands in the Python environment used by ComfyUI:

```bash
python -m pip install 'maturin>=1.9.4,<2'
cd integrations/comfyui_nagisalake
python -m maturin develop --release
```

Use ComfyUI's Python executable for both commands. This is especially
important for portable or embedded ComfyUI installations: PyO3 must build and
link against the same interpreter that loads the custom node.

Then copy `integrations/comfyui_nagisalake` into ComfyUI's
`custom_nodes/comfyui_nagisalake` directory and restart ComfyUI. The
extension build uses the workspace `nagisalake-worker` crate with its
`python` feature and starts a Tokio runtime in a background thread.

Set `NAGISALAKE_WORKER_CONFIG` in the ComfyUI process environment, or put the
path to `examples/nagisalake-worker.toml` in the node's `config_path` input.
The config still controls the Hub token, worker identity, SQLite journal,
ComfyUI URL, and allowlisted workflow capabilities.
