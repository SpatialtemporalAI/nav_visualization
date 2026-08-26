# Woosh Viewer

Cross-platform native operator shell that embeds Rerun 0.36.1 and keeps robot
commands on an independent HTTP control channel.

## Start the robot backend

```bash
conda activate nav
export LD_LIBRARY_PATH="$CONDA_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
cd /home/unitree/workspace/Woosh_robot/src
python run_ros.py
```

The backend starts both the low-rate control API and the Rerun data service.

### Remote sidecar mode for an already-running navigation service

Do not start a second `run_ros.py` beside an existing navigation instance. When
the robot is already running the legacy WebViz backend on port `8008`, use the
sidecar on the remote operator computer instead. Replace the address below with
the robot's LAN address:

```bash
cd /home/unitree/workspace/R-nav/src
uv run --project ../rerun_bridge --extra sidecar --locked python run_rerun_sidecar.py \
  --upstream http://192.168.123.161:8008 \
  --control-port 8010 \
  --rerun-port 9876
```

The robot continues to run only its existing navigation service. The remote
sidecar consumes `/viz/ws`, downloads announced images, converts and records
Rerun locally, and binds its compatibility API to loopback by default. Configure
the operator with `robot_ip = "127.0.0.1"`, `control_port = 8010`, and
`rerun_port = 9876`. Operator actions received locally on `8010` are forwarded
to the robot's existing `8008`; the sidecar sends no navigation command during
startup or normal observation.

See [`docs/rerun_sidecar.md`](../../docs/rerun_sidecar.md) for the data flow,
installation prerequisites, and verification procedure.

## Run

```bash
cargo run --release -- --robot-ip 192.168.123.161
```

Command-line flags override `woosh-viewer.toml`. Packaged builds look for that
file beside the executable, so Windows operators can configure the robot IP and
then launch the viewer by double-clicking the executable.

The left sidebar also has a **连接设置** panel for changing the service host,
control port, and Rerun port while the viewer is running. **应用并连接** switches
both connections without sending a navigation command. Leave **保存到
woosh-viewer.toml** enabled to reuse those values at the next launch. Late HTTP
responses from the previous endpoint are ignored after a switch.

In remote sidecar mode, the service host in this panel is normally `127.0.0.1`.
It identifies the sidecar on the Windows computer, not the physical robot. Set
the physical robot address with `run-sidecar-windows.ps1 -RobotIp ...`; use
`-RobotPort` when its existing WebViz service is not on port `8008`.

The default endpoints are:

- Rerun: `rerun+http://<robot-ip>:9876/proxy`
- Control: `http://<robot-ip>:8008`

The current robot backend already implements the control routes used here:

- `GET /viz/api/operator/status` (one small response per second)
- `GET /viz/api/replay/tasks?limit=20` (only when replay is opened/refreshed)
- `GET /viz/api/replay/tasks/<task-id>/recording.rrd` (only for the selected replay)
- `GET /viz/api/performance` (every two seconds only while its panel is open)
- `POST /viz/api/navigation/text`
- `POST /viz/api/navigation/stop`
- `GET|POST /viz/api/dynamic-map/recording`
- `GET /viz/api/map/metadata`

High-rate pose, planner, image, and dynamic-map data travel through Rerun only.
The native viewer deliberately does not connect to `/viz/ws`, avoiding a second
JSON copy of the visualization stream.

Task playback remains entirely inside Rerun: selecting a history item loads its
`.rrd` recording and Rerun's native timeline provides play, pause, seek, speed,
and per-stream inspection. The side panel also provides native light/dark/system
theme selection. Closing replay or performance panels stops their optional HTTP
traffic.

## Windows build

To copy only the Windows-relevant source instead of the full robot repository,
generate the allow-listed transfer bundle on the robot or development machine:

```bash
python operator/package_windows_bundle.py
```

Copy `dist/woosh-windows-source.zip` to Windows and follow its
`README-WINDOWS.md`. The archive excludes `.git`, maps, ROS/navigation code,
tests, logs, caches, and captures, and verifies every included file against an
internal SHA-256 manifest.

Install Rust 1.95 with the MSVC target and Visual Studio 2022 Build Tools, then
run from PowerShell:

```powershell
.\build-windows.ps1
```

The generated `dist/windows-x64` directory is the operator package. Edit
`woosh-viewer.toml` there and double-click `woosh-viewer.exe`. The viewer itself
does not require Rust, Python, Node.js, or ROS 2 on the target PC. Remote sidecar
mode additionally uses the locked uv environment described above; it never
installs anything on the robot.

The build script removes Cargo's large `target` cache after copying the package.
Developers who expect frequent incremental rebuilds can retain it with:

```powershell
.\build-windows.ps1 -KeepBuildCache
```

## Version policy

Rerun's native viewer extension API is unstable. Keep both the robot bridge and
this application pinned to Rerun 0.36.1 until an intentional migration is made.

## Local UI smoke test

With the repository's `rerun_bridge` environment installed, generate a sample
recording and run the optional mock control endpoint:

```bash
../../rerun_bridge/.venv/bin/python demo/create_sample_recording.py
../../rerun_bridge/.venv/bin/python demo/create_sample_recording.py \
  /tmp/woosh-viewer-replay.rrd viewer-replay
python3 demo/mock_control_server.py
cargo run -- --control-port 18008 --rerun-url /tmp/woosh-viewer-demo.rrd
```
