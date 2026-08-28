# Remote Rerun sidecar mode

> Legacy development path. Current Windows releases integrate this data path
> directly into the Rust Viewer and do not package or launch the Python sidecar.

The remote sidecar adds Rerun to an already-running navigation deployment
without installing Rerun on the robot or starting a second navigation process.
It is intended for robots whose existing `robot_nav` service exposes the legacy
WebViz API on port `8008`.

## Runtime boundaries

```text
robot
  existing robot_nav:8008
    ├─ /viz/ws                  high-rate state, read only
    ├─ /viz/api/frame/...       camera images, read only
    ├─ /viz/api/map/image       map image, read only
    └─ navigation POST routes   only explicit operator actions
                    |
                    | robot LAN
                    v
remote operator computer
  run_rerun_sidecar.py
    ├─ 127.0.0.1:8010           operator compatibility API
    ├─ 127.0.0.1:9876           local Rerun stream
    └─ log/rerun_sidecar_history/*.rrd
                    |
                    v
  woosh-viewer
```

The robot only runs its existing navigation service. `run_rerun_sidecar.py`
does not import `rclpy`, `NodeManager`, or `run_ros.py` and therefore cannot
create competing ROS nodes, claim sensors, or control hardware by itself.

Normal sidecar traffic is read-only:

- one WebSocket connection to the robot's `/viz/ws`;
- one map-metadata check every five seconds;
- a map-image download only when map metadata changes;
- only the newest pending version of each announced camera stream is fetched.

A POST reaches the robot only after an operator explicitly submits navigation,
stops navigation, or changes dynamic-map recording.

## Performance behavior

Rerun conversion, NumPy work, gRPC serving, `.rrd` writing, and visualization all
run on the remote computer. The robot only pays for its existing WebViz JSON and
image responses.

The sidecar bounds memory and disk overhead by:

- keeping at most eight decoded WebSocket messages in the client library;
- replacing pending image work with the newest version per stream;
- caching unchanged map metadata and map image;
- writing camera data only to `.rrd`, without a second legacy frame copy;
- writing task metadata and one final snapshot, without duplicating every pose
  and planner update into `events.jsonl`.

If the remote network cannot keep up with WebViz, live frames can be skipped in
favor of recent data. Navigation execution remains independent.

## Remote computer prerequisites

Prepare the Rerun 0.36.1 environment on the remote computer, not on the robot:

```bash
cd /path/to/nav_visualization
uv sync --project ./rerun_bridge --extra sidecar --locked
```

The locked `sidecar` extra installs FastAPI, Requests, Uvicorn, and WebSockets in
the same environment as Rerun. The remote sidecar therefore runs Rerun directly
without a second Python worker or IPC hop. Windows is supported; replay pruning
uses a process-local lock there and a process/file lock combination on Linux.

## Start on the remote computer

Replace the address with the robot's LAN address:

```bash
cd /path/to/nav_visualization/src
uv run --project ../rerun_bridge --extra sidecar --locked python run_rerun_sidecar.py \
  --upstream http://192.168.1.10:8008 \
  --control-port 8010 \
  --rerun-port 9876
```

The compatibility and Rerun ports bind to `127.0.0.1` by default. They are
available to the operator on the same computer but are not exposed to the LAN.

On Windows PowerShell, the repository includes a launcher that prepares the
locked environment and starts only the remote sidecar:

```powershell
cd C:\path\to\nav_visualization\operator\woosh_viewer
.\run-sidecar-windows.ps1 -RobotIp 192.168.1.10
```

Use `-SkipSync` after the environment has already been prepared and verified.
If the existing robot WebViz service does not use port `8008`, pass its port as
`-RobotPort`; `-ControlPort` and `-RerunPort` select the two local Windows ports.

Task recordings are stored under `log/rerun_sidecar_history` on the remote
computer. If the sidecar attaches during an active task, recording starts from
the initial WebViz snapshot received at connection time.

## Operator configuration

Copy `operator/woosh_viewer/woosh-viewer.sidecar.example.toml` beside the
operator executable as `woosh-viewer.toml`:

```toml
robot_ip = "127.0.0.1"
control_port = 8010
rerun_port = 9876
```

The robot IP belongs only in the sidecar's `--upstream` argument. The operator
connects to the sidecar and Rerun on its own computer.

The Windows operator's left sidebar includes a **连接设置** panel. For the
default sidecar layout, enter service host `127.0.0.1`, control port `8010`, and
Rerun port `9876`, then choose **应用并连接**. The panel can save the values to
`woosh-viewer.toml` beside the executable. Changing these fields reconnects only
the local operator; change the physical robot IP with the PowerShell launcher's
`-RobotIp` option and restart the sidecar.

Do not start `src/run_ros.py` on the robot while its existing `robot_nav` service
is running.
