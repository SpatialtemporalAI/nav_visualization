# Woosh Viewer

Native Windows and macOS operator built around the embedded Rerun 0.36 viewer.

The application now contains the complete former sidecar data path in Rust:

- connects to the robot's `/viz/ws` stream and reconnects automatically;
- fetches map metadata, occupancy images, and announced camera frames;
- logs robot/VPR poses, goals, paths, dynamic occupancy, and events to the
  embedded Rerun server;
- records task `.rrd` files under `%LOCALAPPDATA%\Woosh\rerun-history`;
- sends navigation commands directly to the robot only after an explicit click.

Python, PyArrow, uv, FastAPI, and a second Rerun binary are not needed at runtime.

## Run from source

```powershell
cargo run --release -- --robot-ip 192.168.123.161 --robot-port 8008
```

Command-line flags override `woosh-viewer.toml`. Packaged Windows builds read
that file beside `woosh-viewer.exe`; macOS stores it under
`~/Library/Application Support/Woosh`.

Supported options:

```text
--config FILE
--robot-ip HOST
--robot-port PORT
--rerun-port PORT
--rerun-url URL
--screenshot FILE
```

`--rerun-url` is retained for development and replay diagnostics. Normal live
operation uses the local Rerun server started inside the Viewer.

## Windows release

Install Rust 1.95 with the MSVC target and Visual Studio 2022 Build Tools, then:

```powershell
.\build-windows.ps1 -KeepBuildCache
```

The release script produces `dist/windows-x64` and
`dist/woosh-viewer-windows-x64.zip`. Both contain only the executable and its
configuration file—no Python runtime or external sidecar. The packaged robot IP
is empty, so the first launch opens connection settings for the operator.

## macOS release

On a Mac with Xcode Command Line Tools and Rust 1.95 installed:

```bash
chmod +x build-macos.sh
./build-macos.sh
```

The script produces a Universal Application for Apple Silicon and Intel Macs,
then packages it as `dist/woosh-viewer-macos-universal.zip` and `.dmg`. See
[README-MACOS.md](README-MACOS.md) for installation and signing details.

## Data and control routes

Read-only live data:

- `GET /viz/api/map/metadata`
- `GET /viz/api/map/image`
- `GET` WebSocket `/viz/ws`
- image URLs announced by the WebSocket stream

Explicit operator controls:

- `POST /viz/api/navigation/text`
- `POST /viz/api/navigation/stop`
- `GET|POST /viz/api/dynamic-map/recording`

Rerun is pinned to 0.36.1 because its native viewer extension API is unstable.
