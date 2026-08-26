# Isolated Rerun runtime

The navigation process keeps its existing `numpy<2` dependency. Rerun 0.36.1
runs in a separate uv-managed process because it requires NumPy 2.

Install uv once on the robot, then prepare the locked environment:

```bash
uv sync --project ./rerun_bridge --locked
```

`src/config.yaml` starts the worker with `uv run --locked`. The first launch can
also create the environment automatically, but pre-syncing avoids startup-time
downloads. Port `9876` must be reachable by browsers opening `/viz`.

To use an existing isolated Conda environment instead, replace
`visualization.rerun.bridge_command` with a one-item list containing that
environment's Python executable. The environment must contain
`rerun-sdk==0.36.1`.

## Remote operator sidecar

For a robot that keeps its existing navigation service, install the `sidecar`
extra on the remote Linux or Windows operator computer instead:

```bash
uv sync --project ./rerun_bridge --extra sidecar --locked
```

The remote sidecar runs directly in this environment, so it does not create the
extra isolation process used by robot-side navigation. Rerun conversion, NumPy,
the local gRPC stream, and `.rrd` recording all remain on the remote computer.
